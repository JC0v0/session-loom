use super::{
    adapters,
    config_edit::{jsonc_set, toml_set, upsert_managed_block, upsert_yaml_patch, yaml_patch_body},
    diff::{compare_bytes, ContentDiff},
    model::{
        ConfigEditFormat, ConflictChoice, Diagnostic, MigrationRun, PreviewConflict,
        PreviewConflictItem, PreviewDecision, PreviewFile, PreviewRevision, PreviewSummary,
        Readiness, RenderContribution, RenderPlan, RenderedFile, RenderedFileBody, RunState,
        TargetEnvironment,
    },
    render::render_environment,
    run_store::{private_directory, private_file, sync_directory},
    RunStore,
};
use chrono::Utc;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
use uuid::Uuid;

const PREVIEW_SCHEMA_VERSION: u32 = 1;
const MAX_PREVIEW_RECORD_BYTES: u64 = 16 * 1024 * 1024;

struct BuiltPreview {
    files: Vec<(PreviewFile, Vec<u8>)>,
    conflicts: Vec<PreviewConflict>,
    items: Vec<super::model::RenderedItem>,
    diagnostics: Vec<Diagnostic>,
    ready_to_apply: bool,
}

#[derive(Debug, Clone)]
struct Contribution {
    item_id: String,
    contribution: RenderContribution,
}

/// Create a fresh immutable preview revision from the current source
/// snapshots and target files. The target is never modified here.
pub fn create_preview(
    runs: &RunStore,
    run: &MigrationRun,
    environment: &TargetEnvironment,
    decisions: &[PreviewDecision],
) -> Result<(MigrationRun, PreviewRevision), String> {
    run.validate()?;
    if !matches!(run.state, RunState::Draft | RunState::Previewed) {
        return Err("migration cannot change its preview in the current state".to_string());
    }
    if runs.load(&run.id)? != *run {
        return Err("migration changed; reload before previewing".to_string());
    }
    verify_run_snapshots(runs, run)?;
    let plan = render_environment(runs, run, environment)?;
    let decision_map = valid_decisions(&plan, decisions)?;
    let built = build_preview(runs, run, plan, &decision_map)?;
    let preview_id = Uuid::new_v4().to_string();
    let created_at = Utc::now().to_rfc3339();
    let previews = runs.previews_directory(&run.id)?;
    let staging = previews.join(format!(".staging-{preview_id}"));
    private_directory(&staging)?;
    let result = (|| {
        private_directory(&staging.join("files"))?;
        let mut file_records = vec![];
        for (index, (mut record, bytes)) in built.files.into_iter().enumerate() {
            let name = format!("files/{index:04}-{}.bin", &record.output_hash[..16]);
            let path = staging.join(&name);
            let mut output = private_file(&path, true)?;
            output
                .write_all(&bytes)
                .map_err(|error| error.to_string())?;
            output.sync_all().map_err(|error| error.to_string())?;
            record.private_path = PathBuf::from(name);
            file_records.push(record);
        }
        let revision = PreviewRevision {
            schema_version: PREVIEW_SCHEMA_VERSION,
            id: preview_id.clone(),
            run_id: run.id.clone(),
            run_revision: run
                .revision
                .checked_add(1)
                .ok_or_else(|| "migration revision exhausted".to_string())?,
            created_at: created_at.clone(),
            target: environment.target.clone(),
            environment: environment.clone(),
            decisions: decisions.to_vec(),
            conflicts: built.conflicts,
            items: built.items,
            files: file_records,
            diagnostics: built.diagnostics,
            ready_to_apply: built.ready_to_apply,
        };
        write_private_json(&staging.join("preview.json"), &revision)?;
        sync_directory(&staging.join("files"))?;
        let published = previews.join(&preview_id);
        if published.exists() {
            return Err("preview identity already exists".to_string());
        }
        fs::rename(&staging, &published).map_err(|error| error.to_string())?;
        sync_directory(&previews)?;

        let unresolved_conflicts = revision
            .conflicts
            .iter()
            .filter(|conflict| !conflict.resolved)
            .count();
        let summary = PreviewSummary {
            id: preview_id.clone(),
            run_revision: revision.run_revision,
            created_at,
            file_count: revision.files.len(),
            unresolved_conflicts,
            ready_to_apply: revision.ready_to_apply,
        };
        let mut updated = run.clone();
        updated.revision = run
            .revision
            .checked_add(1)
            .ok_or_else(|| "migration revision exhausted".to_string())?;
        updated.updated_at = Utc::now().to_rfc3339();
        // An unresolved conflict is still a fixed preview: the source
        // snapshot and target are pinned, and apply must reject it until the
        // user decides. Returning to Draft is reserved for selection changes.
        updated.state = RunState::Previewed;
        updated.preview = Some(summary);
        if let Err(error) = runs.update(&updated, run.revision) {
            return Err(format!(
                "preview {preview_id} was saved but its run update failed: {error}"
            ));
        }
        Ok((updated, revision))
    })();
    if result.is_err() && staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn valid_decisions(
    plan: &RenderPlan,
    decisions: &[PreviewDecision],
) -> Result<BTreeMap<String, PreviewDecision>, String> {
    let item_ids: BTreeSet<_> = plan
        .items
        .iter()
        .map(|item| item.item_id.as_str())
        .collect();
    let mut result = BTreeMap::new();
    for decision in decisions {
        if !item_ids.contains(decision.item_id.as_str()) {
            return Err(format!(
                "decision references an item outside this preview: {}",
                decision.item_id
            ));
        }
        if decision.decision_id.trim().is_empty() {
            return Err("preview decision id cannot be empty".to_string());
        }
        if result
            .insert(decision.decision_id.clone(), decision.clone())
            .is_some()
        {
            return Err(format!(
                "duplicate preview decision: {}",
                decision.decision_id
            ));
        }
        if let Some(content) = &decision.merged_content {
            reject_redacted_placeholder(content)?;
        }
        if let Some(value) = &decision.merged_value {
            reject_redacted_placeholder(&value.to_string())?;
        }
        if decision.choice == ConflictChoice::Merge
            && decision.merged_content.is_none()
            && decision.merged_value.is_none()
        {
            return Err(format!(
                "merge decision has no merged content: {}",
                decision.item_id
            ));
        }
        if decision.choice != ConflictChoice::Merge
            && (decision.merged_content.is_some() || decision.merged_value.is_some())
        {
            return Err(format!(
                "only merge decisions may carry replacement content: {}",
                decision.item_id
            ));
        }
    }
    Ok(result)
}

fn reject_redacted_placeholder(value: &str) -> Result<(), String> {
    if value.contains("[redacted]") || value.contains("[redacted-url]") {
        return Err("redacted preview text cannot be used as final content".to_string());
    }
    Ok(())
}

fn build_preview(
    runs: &RunStore,
    run: &MigrationRun,
    plan: RenderPlan,
    decisions: &BTreeMap<String, PreviewDecision>,
) -> Result<BuiltPreview, String> {
    runs.run_directory(&run.id)?;
    let mut files = vec![];
    let mut conflicts = vec![];
    let mut ready = true;
    for rendered in &plan.files {
        validate_target_path(rendered)?;
        let baseline = read_target(&rendered.target_path)?;
        let result = build_file(rendered, baseline, decisions, &plan.conflicts)?;
        conflicts.extend(result.conflicts);
        if result.unresolved {
            ready = false;
        }
        files.push((result.record, result.bytes));
    }
    if plan
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "sourceTargetOverlap")
    {
        ready = false;
    }
    let mut items = plan.items;
    for decision in decisions
        .values()
        .filter(|decision| decision.choice == ConflictChoice::Keep)
    {
        if let Some(item) = items
            .iter_mut()
            .find(|item| item.item_id == decision.item_id)
        {
            if !matches!(item.readiness, Readiness::Failed | Readiness::Unsupported) {
                item.readiness = Readiness::NeedsConfiguration;
                item.message = "已选择保留目标现有内容，尚未验证其可用性".to_string();
                if !item
                    .next_steps
                    .iter()
                    .any(|step| step.contains("目标现有内容"))
                {
                    item.next_steps
                        .push("检查目标现有内容是否满足该工具加载条件".to_string());
                }
                item.action = "keep-target".to_string();
            }
        }
    }
    Ok(BuiltPreview {
        files,
        conflicts,
        items,
        diagnostics: plan.diagnostics,
        ready_to_apply: ready,
    })
}

struct BuiltFile {
    record: PreviewFile,
    bytes: Vec<u8>,
    conflicts: Vec<PreviewConflict>,
    unresolved: bool,
}

fn build_file(
    rendered: &RenderedFile,
    baseline: Option<Vec<u8>>,
    decisions: &BTreeMap<String, PreviewDecision>,
    source_conflicts: &[super::model::SourceConflict],
) -> Result<BuiltFile, String> {
    let kinds: BTreeSet<_> = rendered
        .contributions
        .iter()
        .map(|(_, contribution)| contribution_kind(contribution))
        .collect();
    if kinds.is_empty() {
        return Ok(unchanged_file(rendered, baseline));
    }
    if kinds.len() != 1 {
        return Err(format!(
            "target file has mixed contribution types: {}",
            rendered.target_path.display()
        ));
    }
    match rendered
        .contributions
        .first()
        .map(|(_, contribution)| contribution_kind(contribution))
        .expect("non-empty contributions")
    {
        "wholeFile" => build_whole_file(rendered, baseline, decisions, source_conflicts),
        "managedBlock" => build_managed_blocks(rendered, baseline, decisions),
        "configValue" => build_config_values(rendered, baseline, decisions, source_conflicts),
        "yamlPatch" => build_yaml_patches(rendered, baseline, decisions, source_conflicts),
        value => Err(format!("unknown render contribution kind: {value}")),
    }
}

fn contribution_kind(contribution: &RenderContribution) -> &'static str {
    match contribution {
        RenderContribution::WholeFile { .. } => "wholeFile",
        RenderContribution::ManagedBlock { .. } => "managedBlock",
        RenderContribution::ConfigValue { .. } => "configValue",
        RenderContribution::YamlPatch { .. } => "yamlPatch",
    }
}

fn decision_id(item_id: &str, target_path: &Path, kind: &str, detail: &str) -> String {
    super::model::content_hash(
        format!("{item_id}\0{}\0{kind}\0{detail}", target_path.display()).as_bytes(),
    )
}

fn unchanged_file(rendered: &RenderedFile, baseline: Option<Vec<u8>>) -> BuiltFile {
    let bytes = baseline.clone().unwrap_or_default();
    BuiltFile {
        record: preview_file(
            rendered,
            baseline.as_deref(),
            &bytes,
            rendered.source_item_ids.clone(),
            false,
        ),
        bytes,
        conflicts: vec![],
        unresolved: false,
    }
}

fn build_whole_file(
    rendered: &RenderedFile,
    baseline: Option<Vec<u8>>,
    decisions: &BTreeMap<String, PreviewDecision>,
    source_conflicts: &[super::model::SourceConflict],
) -> Result<BuiltFile, String> {
    let proposals = rendered
        .contributions
        .iter()
        .map(|(item_id, contribution)| match contribution {
            RenderContribution::WholeFile { body, executable } => {
                Ok((item_id.clone(), body_bytes(body)?, *executable))
            }
            _ => Err("expected whole file contribution".to_string()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let source_conflict = source_conflicts
        .iter()
        .find(|conflict| conflict.target_path == rendered.target_path);
    let mut candidates = vec![];
    let mut conflicts = vec![];
    let mut unresolved = false;
    for (item_id, proposed, executable) in &proposals {
        let decision_key = decision_id(item_id, &rendered.target_path, "wholeFile", "");
        let decision = decisions.get(&decision_key);
        let target_differs = baseline.as_deref().is_some_and(|bytes| bytes != proposed);
        let item = conflict_item(
            &decision_key,
            item_id,
            "wholeFile",
            baseline.as_deref(),
            proposed,
        );
        let resolved_view = decision.is_some().then(|| item.clone());
        match decision.map(|decision| decision.choice) {
            Some(ConflictChoice::Keep) => {}
            Some(ConflictChoice::Replace) => candidates.push((*executable, proposed.clone())),
            Some(ConflictChoice::Merge) => {
                let merged = decision
                    .and_then(|decision| decision.merged_content.as_deref())
                    .ok_or_else(|| {
                        format!("binary or whole-file merge needs UTF-8 content: {item_id}")
                    })?;
                reject_redacted_placeholder(merged)?;
                candidates.push((*executable, merged.as_bytes().to_vec()));
            }
            None => {
                if source_conflict.is_some() || target_differs {
                    unresolved = true;
                    conflicts.push(target_conflict(
                        rendered,
                        item.clone(),
                        if source_conflict.is_some() {
                            "sourceSource"
                        } else {
                            "targetChanged"
                        },
                        false,
                    ));
                } else {
                    candidates.push((*executable, proposed.clone()));
                }
            }
        }
        if let Some(resolved_view) = resolved_view {
            if source_conflict.is_none() {
                conflicts.push(target_conflict(
                    rendered,
                    resolved_view,
                    "targetChanged",
                    true,
                ));
            }
        }
    }
    if let Some(conflict) = source_conflict {
        let mut source_resolved = proposals.iter().all(|(item_id, _, _)| {
            decisions.contains_key(&decision_id(
                item_id,
                &rendered.target_path,
                "wholeFile",
                "",
            ))
        });
        if source_resolved {
            source_resolved = candidates.len() <= 1;
        }
        unresolved |= !source_resolved;
        let items = proposals
            .iter()
            .map(|(item_id, proposed, _)| {
                conflict_item(
                    &decision_id(item_id, &rendered.target_path, "wholeFile", ""),
                    item_id,
                    "wholeFile",
                    baseline.as_deref(),
                    proposed,
                )
            })
            .collect();
        conflicts.push(PreviewConflict {
            id: conflict.id.clone(),
            kind: "sourceSource".to_string(),
            target_path: rendered.target_path.clone(),
            item_ids: conflict.item_ids.clone(),
            message: "多个来源写入同一目标文件，需要选择保留、合并或替换".to_string(),
            items,
            resolved: source_resolved,
        });
    }
    if candidates.len() > 1 {
        unresolved = true;
    }
    let (executable, bytes) = if unresolved {
        (false, baseline.clone().unwrap_or_default())
    } else if let Some(candidate) = candidates.pop() {
        candidate
    } else {
        (false, baseline.clone().unwrap_or_default())
    };
    let write = baseline
        .as_deref()
        .map(|baseline| baseline != bytes)
        .unwrap_or(!bytes.is_empty());
    let item_ids = proposals.iter().map(|(item, _, _)| item.clone()).collect();
    let mut record = preview_file(rendered, baseline.as_deref(), &bytes, item_ids, write);
    record.executable = write && executable;
    Ok(BuiltFile {
        record,
        bytes,
        conflicts,
        unresolved,
    })
}

fn build_managed_blocks(
    rendered: &RenderedFile,
    baseline: Option<Vec<u8>>,
    decisions: &BTreeMap<String, PreviewDecision>,
) -> Result<BuiltFile, String> {
    let baseline_bytes = baseline.clone();
    let mut text = baseline
        .as_deref()
        .map(|bytes| {
            String::from_utf8(bytes.to_vec())
                .map_err(|_| "target instruction file is not UTF-8".to_string())
        })
        .transpose()?
        .unwrap_or_default();
    let mut conflicts = vec![];
    let mut unresolved = false;
    for (item_id, contribution) in &rendered.contributions {
        let RenderContribution::ManagedBlock { marker, body } = contribution else {
            return Err("expected managed instruction contribution".to_string());
        };
        let decision_key = decision_id(item_id, &rendered.target_path, "managedBlock", marker);
        let decision = decisions.get(&decision_key);
        let merged = decision
            .and_then(|decision| decision.merged_content.as_deref())
            .unwrap_or(body);
        let candidate = upsert_managed_block(&text, marker, merged)?;
        let existing = managed_block_body(&text, marker);
        let resolved_view = decision.is_some().then(|| {
            conflict_item(
                &decision_key,
                item_id,
                "managedBlock",
                baseline_bytes.as_deref(),
                candidate.as_bytes(),
            )
        });
        match decision.map(|decision| decision.choice) {
            Some(ConflictChoice::Keep) => {}
            Some(ConflictChoice::Replace | ConflictChoice::Merge) => {
                reject_redacted_placeholder(merged)?;
                text = candidate;
            }
            None => {
                if existing.is_some() && candidate != text {
                    unresolved = true;
                    conflicts.push(target_conflict(
                        rendered,
                        conflict_item(
                            &decision_key,
                            item_id,
                            "managedBlock",
                            baseline_bytes.as_deref(),
                            candidate.as_bytes(),
                        ),
                        "targetChanged",
                        false,
                    ));
                } else {
                    text = candidate;
                }
            }
        }
        if let Some(resolved_view) = resolved_view {
            conflicts.push(target_conflict(
                rendered,
                resolved_view,
                "targetChanged",
                true,
            ));
        }
    }
    let bytes = text.into_bytes();
    let write = baseline
        .as_deref()
        .map(|baseline| baseline != bytes)
        .unwrap_or(!bytes.is_empty());
    Ok(BuiltFile {
        record: preview_file(
            rendered,
            baseline.as_deref(),
            &bytes,
            rendered.source_item_ids.clone(),
            write,
        ),
        bytes,
        conflicts,
        unresolved,
    })
}

fn managed_block_body(text: &str, marker: &str) -> Option<String> {
    let start = format!("<!-- session-loom:{marker}:start -->");
    let end = format!("<!-- session-loom:{marker}:end -->");
    let start_index = text.find(&start)?;
    let body_start = start_index + start.len();
    let end_offset = text[body_start..].find(&end)?;
    Some(text[body_start..body_start + end_offset].trim().to_string())
}

fn build_config_values(
    rendered: &RenderedFile,
    baseline: Option<Vec<u8>>,
    decisions: &BTreeMap<String, PreviewDecision>,
    source_conflicts: &[super::model::SourceConflict],
) -> Result<BuiltFile, String> {
    let mut groups: BTreeMap<(ConfigEditFormat, Vec<String>, String), Vec<Contribution>> =
        BTreeMap::new();
    for (item_id, contribution) in &rendered.contributions {
        let RenderContribution::ConfigValue {
            format,
            object_path,
            key,
            value,
        } = contribution
        else {
            return Err("expected config value contribution".to_string());
        };
        groups
            .entry((*format, object_path.clone(), key.clone()))
            .or_default()
            .push(Contribution {
                item_id: item_id.clone(),
                contribution: RenderContribution::ConfigValue {
                    format: *format,
                    object_path: object_path.clone(),
                    key: key.clone(),
                    value: value.clone(),
                },
            });
    }
    let mut conflicts = vec![];
    let mut unresolved = false;
    let mut text = match baseline.as_deref() {
        Some(bytes) => String::from_utf8(bytes.to_vec())
            .map_err(|_| "target configuration is not UTF-8".to_string())?,
        None => String::new(),
    };
    let mut changed = false;
    for ((format, object_path, key), contributions) in groups {
        let current = config_value(&text, format, &object_path, &key)?;
        let source_conflict = contributions.len() > 1
            || source_conflicts.iter().any(|conflict| {
                conflict.target_path == rendered.target_path
                    && conflict.kind == "configValue"
                    && contributions
                        .iter()
                        .all(|item| conflict.item_ids.contains(&item.item_id))
            });
        let mut candidates = vec![];
        for contribution in &contributions {
            let RenderContribution::ConfigValue {
                value,
                object_path,
                key,
                ..
            } = &contribution.contribution
            else {
                unreachable!();
            };
            let detail = format!("{}.{}", object_path.join("."), key);
            let decision_key = decision_id(
                &contribution.item_id,
                &rendered.target_path,
                "configValue",
                &detail,
            );
            let item = conflict_item_value(
                &decision_key,
                &contribution.item_id,
                "configValue",
                current.as_ref(),
                value,
            );
            let resolved_view = decisions.get(&decision_key).is_some().then(|| item.clone());
            match decisions.get(&decision_key).map(|decision| decision.choice) {
                Some(ConflictChoice::Keep) => {}
                Some(ConflictChoice::Replace) => candidates.push(value.clone()),
                Some(ConflictChoice::Merge) => {
                    let value = decisions[&decision_key]
                        .merged_value
                        .clone()
                        .ok_or_else(|| {
                            format!("config merge needs mergedValue: {}", contribution.item_id)
                        })?;
                    candidates.push(value);
                }
                None => {
                    if current.as_ref() == Some(value) {
                        if let Some(resolved_view) = resolved_view {
                            conflicts.push(target_conflict(
                                rendered,
                                resolved_view,
                                "targetChanged",
                                true,
                            ));
                        }
                        continue;
                    }
                    if source_conflict || current.is_some() {
                        unresolved = true;
                        conflicts.push(target_conflict(
                            rendered,
                            item,
                            if source_conflict {
                                "sourceSource"
                            } else {
                                "targetChanged"
                            },
                            false,
                        ));
                        continue;
                    } else {
                        candidates.push(value.clone());
                    }
                }
            }
            if let Some(resolved_view) = resolved_view {
                conflicts.push(target_conflict(
                    rendered,
                    resolved_view,
                    "targetChanged",
                    true,
                ));
            }
        }
        if candidates.len() > 1 {
            unresolved = true;
        }
        if candidates.len() == 1 {
            text = edit_config(&text, format, &object_path, &key, &candidates[0])?;
            changed = true;
        }
    }
    let bytes = text.into_bytes();
    let write = baseline
        .as_deref()
        .map(|baseline| baseline != bytes)
        .unwrap_or(changed);
    Ok(BuiltFile {
        record: preview_file(
            rendered,
            baseline.as_deref(),
            &bytes,
            rendered.source_item_ids.clone(),
            write,
        ),
        bytes,
        conflicts,
        unresolved,
    })
}

fn build_yaml_patches(
    rendered: &RenderedFile,
    baseline: Option<Vec<u8>>,
    decisions: &BTreeMap<String, PreviewDecision>,
    source_conflicts: &[super::model::SourceConflict],
) -> Result<BuiltFile, String> {
    let baseline_bytes = baseline.clone();
    let mut text = baseline
        .as_deref()
        .map(|bytes| {
            String::from_utf8(bytes.to_vec())
                .map_err(|_| "target YAML configuration is not UTF-8".to_string())
        })
        .transpose()?
        .unwrap_or_else(|| "[]\n".to_string());
    let mut conflicts = vec![];
    let mut unresolved = false;
    for (item_id, contribution) in &rendered.contributions {
        let RenderContribution::YamlPatch { marker, patch } = contribution else {
            return Err("expected YAML patch contribution".to_string());
        };
        let current = yaml_patch_body(&text, marker);
        let source_conflict = source_conflicts.iter().any(|conflict| {
            conflict.target_path == rendered.target_path
                && conflict.kind == "configValue"
                && conflict.item_ids.contains(item_id)
        });
        let decision_key = decision_id(item_id, &rendered.target_path, "yamlPatch", marker);
        let decision = decisions.get(&decision_key);
        let merged = decision
            .and_then(|decision| decision.merged_content.as_deref())
            .unwrap_or(patch);
        match decision.map(|decision| decision.choice) {
            Some(ConflictChoice::Keep) => {}
            Some(ConflictChoice::Replace | ConflictChoice::Merge) => {
                reject_redacted_placeholder(merged)?;
                text = upsert_yaml_patch(&text, marker, merged)?;
            }
            None => {
                if current.as_ref() == Some(patch) {
                    continue;
                }
                if source_conflict || current.is_some() {
                    unresolved = true;
                    conflicts.push(target_conflict(
                        rendered,
                        conflict_item(
                            &decision_key,
                            item_id,
                            "yamlPatch",
                            baseline_bytes.as_deref(),
                            patch.as_bytes(),
                        ),
                        if source_conflict {
                            "sourceSource"
                        } else {
                            "targetChanged"
                        },
                        false,
                    ));
                } else {
                    text = upsert_yaml_patch(&text, marker, patch)?;
                }
            }
        }
        if decision.is_some() {
            conflicts.push(target_conflict(
                rendered,
                conflict_item(
                    &decision_key,
                    item_id,
                    "yamlPatch",
                    baseline_bytes.as_deref(),
                    patch.as_bytes(),
                ),
                "targetChanged",
                true,
            ));
        }
    }
    let bytes = text.into_bytes();
    let write = baseline
        .as_deref()
        .map(|baseline| baseline != bytes)
        .unwrap_or(true);
    Ok(BuiltFile {
        record: preview_file(
            rendered,
            baseline.as_deref(),
            &bytes,
            rendered.source_item_ids.clone(),
            write,
        ),
        bytes,
        conflicts,
        unresolved,
    })
}

fn config_value(
    text: &str,
    format: ConfigEditFormat,
    object_path: &[String],
    key: &str,
) -> Result<Option<Value>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let value = match format {
        ConfigEditFormat::Jsonc => adapters::parse_json(text)?,
        ConfigEditFormat::Toml => adapters::parse_toml(text)?,
    };
    let mut current = Some(&value);
    for segment in object_path {
        let Some(next) = current.and_then(|value| value.get(segment)) else {
            return Ok(None);
        };
        current = Some(next);
    }
    Ok(current.and_then(|value| value.get(key)).cloned())
}

fn edit_config(
    text: &str,
    format: ConfigEditFormat,
    object_path: &[String],
    key: &str,
    value: &Value,
) -> Result<String, String> {
    let segments: Vec<&str> = object_path.iter().map(String::as_str).collect();
    match format {
        ConfigEditFormat::Jsonc => {
            let text = if text.trim().is_empty() {
                "{\n}\n"
            } else {
                text
            };
            jsonc_set(text, &segments, key, value)
        }
        ConfigEditFormat::Toml => toml_set(text, &segments, key, value),
    }
}

fn body_bytes(body: &RenderedFileBody) -> Result<Vec<u8>, String> {
    match body {
        RenderedFileBody::Bytes(bytes) => Ok(bytes.clone()),
        RenderedFileBody::Copy {
            snapshot_path,
            content_hash,
        } => {
            let bytes = fs::read(snapshot_path).map_err(|error| error.to_string())?;
            if content_hash != &super::model::content_hash(&bytes) {
                return Err(format!(
                    "snapshot changed during preview: {}",
                    snapshot_path.display()
                ));
            }
            Ok(bytes)
        }
    }
}

fn conflict_item(
    decision_id: &str,
    item_id: &str,
    kind: &str,
    current: Option<&[u8]>,
    proposed: &[u8],
) -> PreviewConflictItem {
    PreviewConflictItem {
        decision_id: decision_id.to_string(),
        item_id: item_id.to_string(),
        contribution_kind: kind.to_string(),
        current_hash: current.map(super::model::content_hash),
        proposed_hash: super::model::content_hash(proposed),
        diff: compare_bytes(current, proposed),
    }
}

fn conflict_item_value(
    decision_id: &str,
    item_id: &str,
    kind: &str,
    current: Option<&Value>,
    proposed: &Value,
) -> PreviewConflictItem {
    let current = current.and_then(|value| serde_json::to_vec_pretty(value).ok());
    let proposed = serde_json::to_vec_pretty(proposed).unwrap_or_default();
    let mut item = conflict_item(decision_id, item_id, kind, current.as_deref(), &proposed);
    if let ContentDiff::Binary { .. } = item.diff {
        item.diff = compare_bytes(current.as_deref(), &proposed);
    }
    item
}

fn target_conflict(
    rendered: &RenderedFile,
    item: PreviewConflictItem,
    kind: &str,
    resolved: bool,
) -> PreviewConflict {
    PreviewConflict {
        id: super::model::content_hash(
            format!(
                "{}\0{}\0{}",
                rendered.target_path.display(),
                item.item_id,
                kind
            )
            .as_bytes(),
        ),
        kind: kind.to_string(),
        target_path: rendered.target_path.clone(),
        item_ids: vec![item.item_id.clone()],
        message: if kind == "sourceSource" {
            "多个来源提供不同内容，需要选择处理方式".to_string()
        } else {
            "目标内容与预览不同，需要选择保留、合并或替换".to_string()
        },
        items: vec![item],
        resolved,
    }
}

fn preview_file(
    rendered: &RenderedFile,
    baseline: Option<&[u8]>,
    output: &[u8],
    item_ids: Vec<String>,
    changed: bool,
) -> PreviewFile {
    PreviewFile {
        target_scope: rendered.target_scope,
        scope_root: rendered.scope_root.clone(),
        target_path: rendered.target_path.clone(),
        private_path: PathBuf::new(),
        item_ids,
        target_existed: baseline.is_some(),
        target_hash: baseline.map(super::model::content_hash),
        output_hash: super::model::content_hash(output),
        output_size: output.len() as u64,
        executable: rendered.executable,
        changed,
        diff: compare_bytes(baseline, output),
    }
}

fn validate_target_path(rendered: &RenderedFile) -> Result<(), String> {
    if !rendered.target_path.is_absolute()
        || !rendered.target_path.starts_with(&rendered.scope_root)
    {
        return Err("preview target path leaves its selected scope".to_string());
    }
    if rendered
        .target_path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("preview target path contains a parent traversal".to_string());
    }
    if fs::symlink_metadata(&rendered.target_path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err("preview target is a symbolic link".to_string());
    }
    let scope_root = if rendered.scope_root.exists() {
        rendered
            .scope_root
            .canonicalize()
            .map_err(|error| error.to_string())?
    } else {
        rendered.scope_root.clone()
    };
    let mut ancestor = rendered.target_path.parent();
    while let Some(path) = ancestor {
        if !path.starts_with(&rendered.scope_root) {
            break;
        }
        if path.exists() {
            let resolved = path.canonicalize().map_err(|error| error.to_string())?;
            if !resolved.starts_with(&scope_root) {
                return Err("preview target parent resolves outside its selected scope".to_string());
            }
            break;
        }
        ancestor = path.parent();
    }
    Ok(())
}

pub(crate) fn read_target(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(format!("target is not a regular file: {}", path.display()))
        }
        Ok(_) => fs::read(path).map(Some).map_err(|error| error.to_string()),
    }
}

pub(crate) fn verify_run_snapshots(runs: &RunStore, run: &MigrationRun) -> Result<(), String> {
    let directory = runs.run_directory(&run.id)?;
    for asset in &run.assets {
        for file in &asset.files {
            verify_hashed_file(
                &directory.join(&file.snapshot_path),
                &file.content_hash,
                "source snapshot changed; refresh the preview",
            )?;
        }
    }
    for session in &run.sessions {
        verify_hashed_file(
            &directory.join(&session.snapshot_path),
            &session.content_hash,
            "session snapshot changed; refresh the preview",
        )?;
    }
    Ok(())
}

fn verify_hashed_file(path: &Path, expected_hash: &str, message: &str) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("{message}: {}", path.display()));
    }
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    if super::model::content_hash(&bytes) != expected_hash {
        return Err(format!("{message}: {}", path.display()));
    }
    Ok(())
}

fn write_private_json(path: &Path, value: &PreviewRevision) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    let mut output = private_file(path, true)?;
    output
        .write_all(&bytes)
        .map_err(|error| error.to_string())?;
    output.sync_all().map_err(|error| error.to_string())
}

pub fn load_preview(
    runs: &RunStore,
    run_id: &str,
    preview_id: &str,
) -> Result<PreviewRevision, String> {
    validate_preview_id(preview_id)?;
    let previews = runs.previews_directory(run_id)?;
    let directory = previews.join(preview_id);
    let metadata = fs::symlink_metadata(&directory).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("preview directory is not a regular directory".to_string());
    }
    let path = directory.join("preview.json");
    let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_PREVIEW_RECORD_BYTES
    {
        return Err("preview record is not a valid private file".to_string());
    }
    let mut bytes = vec![];
    fs::File::open(&path)
        .map_err(|error| error.to_string())?
        .take(MAX_PREVIEW_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_PREVIEW_RECORD_BYTES {
        return Err("preview record is too large".to_string());
    }
    let preview: PreviewRevision =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if preview.schema_version != PREVIEW_SCHEMA_VERSION
        || preview.id != preview_id
        || preview.run_id != run_id
    {
        return Err("preview record identity does not match its location".to_string());
    }
    Ok(preview)
}

pub(crate) fn read_preview_file(
    runs: &RunStore,
    preview: &PreviewRevision,
    file: &PreviewFile,
) -> Result<Vec<u8>, String> {
    let directory = runs.previews_directory(&preview.run_id)?.join(&preview.id);
    super::model::validate_resource_path(&file.private_path)?;
    let path = directory.join(&file.private_path);
    let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("preview output is not a regular private file".to_string());
    }
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    if super::model::content_hash(&bytes) != file.output_hash {
        return Err("preview output changed after it was created".to_string());
    }
    Ok(bytes)
}

pub fn verify_preview(
    runs: &RunStore,
    run_id: &str,
    preview_id: &str,
) -> Result<(MigrationRun, PreviewRevision), String> {
    let run = runs.load(run_id)?;
    let preview = load_preview(runs, run_id, preview_id)?;
    if run.preview.as_ref().map(|preview| preview.id.as_str()) != Some(preview_id)
        || run.revision != preview.run_revision
    {
        return Err("preview is no longer the current migration revision".to_string());
    }
    verify_run_snapshots(runs, &run)?;
    for file in &preview.files {
        let current = read_target(&file.target_path)?;
        let current_hash = current.as_deref().map(super::model::content_hash);
        if current_hash != file.target_hash {
            return Err(format!(
                "target changed after preview: {}",
                file.target_path.display()
            ));
        }
        let output = read_preview_file(runs, &preview, file)?;
        if super::model::content_hash(&output) != file.output_hash {
            return Err(format!(
                "preview output changed: {}",
                file.target_path.display()
            ));
        }
    }
    Ok((run, preview))
}

fn validate_preview_id(id: &str) -> Result<(), String> {
    let parsed = Uuid::parse_str(id).map_err(|_| "invalid preview id".to_string())?;
    if parsed.to_string() == id {
        Ok(())
    } else {
        Err("invalid preview id".to_string())
    }
}

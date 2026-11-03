/// Discover native Markdown @imports without treating examples or email
/// addresses as file references. The caller resolves and authorizes paths.
pub(crate) fn import_paths(text: &str) -> Vec<String> {
    let mut result = vec![];
    let mut fence = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            let marker = trimmed.chars().next();
            if fence == marker {
                fence = None;
            } else if fence.is_none() {
                fence = marker;
            }
            continue;
        }
        if fence.is_some() || line.starts_with("    ") || line.starts_with('\t') {
            continue;
        }
        let chars: Vec<_> = line.chars().collect();
        let mut index = 0;
        let mut code = false;
        while index < chars.len() {
            if chars[index] == '`' {
                code = !code;
                index += 1;
                continue;
            }
            if code || chars[index] != '@' || (index > 0 && !chars[index - 1].is_whitespace()) {
                index += 1;
                continue;
            }
            index += 1;
            let quote = chars
                .get(index)
                .copied()
                .filter(|value| matches!(value, '\'' | '"'));
            if quote.is_some() {
                index += 1;
            }
            let start = index;
            while index < chars.len()
                && (if let Some(quote) = quote {
                    chars[index] != quote
                } else {
                    !chars[index].is_whitespace() && chars[index] != '`'
                })
            {
                index += 1;
            }
            let path: String = chars[start..index].iter().collect();
            if !path.is_empty() && !result.contains(&path) {
                result.push(path);
            }
            if result.len() >= 512 {
                return result;
            }
        }
    }
    result
}

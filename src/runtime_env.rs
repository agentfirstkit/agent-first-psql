pub fn nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

pub fn colon_list(name: &str) -> Vec<String> {
    nonempty(name)
        .map(|value| {
            value
                .split(':')
                .filter(|part| !part.is_empty())
                .map(std::string::ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

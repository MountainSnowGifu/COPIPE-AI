pub fn build_system_prompt(root: &std::path::Path) -> String {
    let template = include_str!("templates/system_prompt.md");
    template.replace("{root}", &root.display().to_string())
}

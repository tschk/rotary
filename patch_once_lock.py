with open("src/models.rs", "r") as f:
    content = f.read()

target = """fn builtin_models() -> Vec<ModelInfo> {
    serde_json::from_str(include_str!("builtin_models.json")).expect("failed to parse builtin models")
}"""

replacement = """fn builtin_models() -> &'static [ModelInfo] {
    static MODELS: std::sync::OnceLock<Vec<ModelInfo>> = std::sync::OnceLock::new();
    MODELS.get_or_init(|| {
        serde_json::from_str(include_str!("builtin_models.json"))
            .expect("failed to parse builtin models")
    })
}"""

content = content.replace(target, replacement)

target2 = """    pub fn load() -> Self {
        let mut models = HashMap::new();
        for model in builtin_models() {
            models.insert(model.id.clone(), model);
        }"""

replacement2 = """    pub fn load() -> Self {
        let mut models = HashMap::new();
        for model in builtin_models() {
            models.insert(model.id.clone(), model.clone());
        }"""

content = content.replace(target2, replacement2)

with open("src/models.rs", "w") as f:
    f.write(content)

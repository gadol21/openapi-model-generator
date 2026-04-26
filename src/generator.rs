use std::collections::HashSet;
use std::sync::OnceLock;

use crate::{
    models::{
        CompositionModel, EnumModel, Model, ModelType, RequestModel, ResponseModel, TypeAliasModel,
        UnionModel, UnionType,
    },
    Result,
};

bitflags::bitflags! {
    struct RequiredUses: u8 {
        const UUID = 0b00000001;
        const DATETIME = 0b00000010;
        const DATE = 0b00000100;
        const RAW_VALUE = 0b00001000;
    }

    /// Choose what type of structs you want to generate:
    ///  - Models (generated always)
    ///  - Requests (optional)
    ///  - Responses (optional)
    pub struct GenerateMode: u8 {
        /// Models will be always include to output
        const MODELS = 0;
        /// Additional includes request structs to output
        const REQUESTS = 1 << 0;
        /// Additional includes response structs to output
        const RESPONSES = 1 << 1;
        /// Outputs all possible structs: models, request and response structs
        const ALL = Self::REQUESTS.bits() | Self::RESPONSES.bits();
    }
}

impl Default for GenerateMode {
    fn default() -> Self {
        Self::ALL
    }
}

static HDR: OnceLock<String> = OnceLock::new();

fn create_header() -> String {
    HDR.get_or_init(|| {
        format!(
            r#"
//!
//! Generated from an OAS specification by {}(v{})
//! Requires serde_json with the "raw_value" feature enabled.
//!

"#,
            option_env!("CARGO_PKG_NAME").unwrap_or("openapi-model-generator"),
            option_env!("CARGO_PKG_VERSION").unwrap_or("unknown")
        )
    })
    .clone()
}

const RUST_RESERVED_KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", "abstract", "become", "box", "do", "final", "gen", "macro", "override", "priv", "try",
    "typeof", "unsized", "virtual", "yield",
];

const EMPTY_RESPONSE_NAME: &str = "UnknownResponse";
const EMPTY_REQUEST_NAME: &str = "UnknownRequest";

fn is_reserved_word(string_to_check: &str) -> bool {
    RUST_RESERVED_KEYWORDS.contains(&string_to_check.to_lowercase().as_str())
}

fn generate_description_docs(
    description: &Option<String>,
    fallback_str: &str,
    indent: &str,
) -> String {
    let mut output = String::new();
    if let Some(desc) = description {
        for line in desc.lines() {
            output.push_str(&format!("{}/// {}\n", indent, line.trim()));
        }
    } else if !fallback_str.is_empty() {
        output.push_str(&format!("{}/// {}\n", indent, fallback_str));
    }

    output
}

fn to_snake_case(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();

    let mut snake = String::new();

    for (i, c) in cleaned.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i != 0 {
                snake.push('_');
            }
            snake.push(c.to_ascii_lowercase());
        } else {
            snake.push(c);
        }
    }
    snake = snake.replace("__", "_");

    if snake == "self" {
        snake.push('_');
    }

    if snake
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        snake = format!("_{snake}");
    }

    snake
}

/// Checks if custom attributes contain a derive attribute
fn has_custom_derive(custom_attrs: &Option<Vec<String>>) -> bool {
    if let Some(attrs) = custom_attrs {
        attrs
            .iter()
            .any(|attr| attr.trim().starts_with("#[derive("))
    } else {
        false
    }
}

/// Checks if custom attributes contain a serde attribute
fn has_custom_serde(custom_attrs: &Option<Vec<String>>) -> bool {
    if let Some(attrs) = custom_attrs {
        attrs.iter().any(|attr| attr.trim().starts_with("#[serde("))
    } else {
        false
    }
}

/// Generates custom attributes from x-rust-attrs
fn generate_custom_attrs(custom_attrs: &Option<Vec<String>>) -> String {
    if let Some(attrs) = custom_attrs {
        attrs
            .iter()
            .map(|attr| format!("{attr}\n"))
            .collect::<String>()
    } else {
        String::new()
    }
}

fn types_needing_lifetime(models: &[ModelType]) -> HashSet<String> {
    let mut deps: Vec<(String, Vec<String>)> = Vec::new();
    let mut needs_lifetime: HashSet<String> = HashSet::new();

    for model in models {
        match model {
            ModelType::Struct(m) => {
                let mut field_types = Vec::new();
                for field in &m.fields {
                    if field.field_type.contains("RawValue") {
                        needs_lifetime.insert(m.name.clone());
                    } else {
                        field_types.push(field.field_type.clone());
                    }
                }
                deps.push((m.name.clone(), field_types));
            }
            ModelType::Composition(c) => {
                let mut field_types = Vec::new();
                for field in &c.all_fields {
                    if field.field_type.contains("RawValue") {
                        needs_lifetime.insert(c.name.clone());
                    } else {
                        field_types.push(field.field_type.clone());
                    }
                }
                deps.push((c.name.clone(), field_types));
            }
            ModelType::Union(u) => {
                let mut variant_types = Vec::new();
                for variant in &u.variants {
                    if let Some(pt) = &variant.primitive_type {
                        if pt.contains("RawValue") {
                            needs_lifetime.insert(u.name.clone());
                        }
                    } else {
                        variant_types.push(variant.name.clone());
                    }
                    for field in &variant.fields {
                        if field.field_type.contains("RawValue") {
                            needs_lifetime.insert(u.name.clone());
                        } else {
                            variant_types.push(field.field_type.clone());
                        }
                    }
                }
                deps.push((u.name.clone(), variant_types));
            }
            ModelType::TypeAlias(t) => {
                if t.target_type.contains("RawValue") {
                    needs_lifetime.insert(t.name.clone());
                }
                deps.push((t.name.clone(), vec![t.target_type.clone()]));
            }
            ModelType::Enum(_) => {}
        }
    }

    loop {
        let mut changed = false;
        for (name, ref_types) in &deps {
            if !needs_lifetime.contains(name) {
                for ref_type in ref_types {
                    if needs_lifetime.contains(ref_type) {
                        needs_lifetime.insert(name.clone());
                        changed = true;
                        break;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    needs_lifetime
}

fn field_needs_borrow(field_type: &str, needs_lifetime: &HashSet<String>) -> bool {
    field_type.contains("RawValue") || needs_lifetime.contains(field_type)
}

fn type_with_lifetime(field_type: &str, needs_lifetime: &HashSet<String>) -> String {
    if needs_lifetime.contains(field_type) && !field_type.contains("'a") {
        format!("{field_type}<'a>")
    } else {
        field_type.to_string()
    }
}

pub fn generate_models(
    models: &[ModelType],
    requests: &[RequestModel],
    responses: &[ResponseModel],
    mode: GenerateMode,
) -> Result<String> {
    let lifetime_types = types_needing_lifetime(models);

    let mut models_code = String::new();
    let mut required_uses = RequiredUses::empty();
    let mut needs_validator = false;

    for model_type in models {
        match model_type {
            ModelType::Struct(model) => {
                models_code.push_str(&generate_model(
                    model,
                    &mut required_uses,
                    &mut needs_validator,
                    &lifetime_types,
                )?);
            }
            ModelType::Union(union) => {
                models_code.push_str(&generate_union(union, &lifetime_types)?);
            }
            ModelType::Composition(comp) => {
                models_code.push_str(&generate_composition(
                    comp,
                    &mut required_uses,
                    &lifetime_types,
                )?);
            }
            ModelType::Enum(enum_model) => {
                models_code.push_str(&generate_enum(enum_model)?);
            }
            ModelType::TypeAlias(type_alias) => {
                models_code.push_str(&generate_type_alias(type_alias, &lifetime_types)?);
            }
        }
    }

    if mode.contains(GenerateMode::REQUESTS) {
        for request in requests {
            models_code.push_str(&generate_request_model(request, &lifetime_types)?);
        }
    }

    if mode.contains(GenerateMode::RESPONSES) {
        for response in responses {
            models_code.push_str(&generate_response_model(response, &lifetime_types)?);
        }
    }

    let needs_uuid = required_uses.contains(RequiredUses::UUID);
    let needs_datetime = required_uses.contains(RequiredUses::DATETIME);
    let needs_date = required_uses.contains(RequiredUses::DATE);
    let needs_raw_value = required_uses.contains(RequiredUses::RAW_VALUE);

    let mut output = create_header();
    output.push_str("use serde::{Serialize, Deserialize};\n");

    if needs_raw_value {
        output.push_str("use serde_json::value::RawValue;\n");
    }

    if needs_uuid {
        output.push_str("use uuid::Uuid;\n");
    }

    if needs_validator {
        output.push_str("use validator::Validator;\n");
    }

    if needs_datetime || needs_date {
        output.push_str("use chrono::{");
        let mut chrono_imports = Vec::new();
        if needs_datetime {
            chrono_imports.push("DateTime");
        }
        if needs_date {
            chrono_imports.push("NaiveDate");
        }
        if needs_datetime {
            chrono_imports.push("Utc");
        }
        output.push_str(&chrono_imports.join(", "));
        output.push_str("};\n");
    }

    output.push('\n');
    output.push_str(&models_code);

    Ok(output)
}

/// Generate validator attributes based on validation rules
fn generate_validator_attrs(rules: &crate::models::ValidationRules, field_type: &str) -> String {
    let mut attrs = String::new();

    match field_type {
        "String" | "str" | "Option<String>" | "Option<str>" => {
            let mut length_attrs = Vec::new();
            if let Some(min) = rules.min_length {
                length_attrs.push(format!("min = {}", min));
            }
            if let Some(max) = rules.max_length {
                length_attrs.push(format!("max = {}", max));
            }
            if !length_attrs.is_empty() {
                attrs.push_str(&format!(
                    "    #[validate(length({}))]\n",
                    length_attrs.join(", ")
                ));
            }

            if rules.email {
                attrs.push_str("    #[validate(email)]\n");
            }

            if rules.url {
                attrs.push_str("    #[validate(url)]\n");
            }

            if let Some(pattern) = &rules.pattern {
                attrs.push_str(&format!("    #[regex(pattern = r\"{}\")]\n", pattern));
            }
        }
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "f32" | "f64"
        | "Option<i8>" | "Option<i16>" | "Option<i32>" | "Option<i64>" | "Option<u8>"
        | "Option<u16>" | "Option<u32>" | "Option<u64>" | "Option<f32>" | "Option<f64>" => {
            let mut range_attrs = Vec::new();
            if let Some(min) = rules.minimum {
                range_attrs.push(format!("min = {}", min));
            }
            if let Some(max) = rules.maximum {
                range_attrs.push(format!("max = {}", max));
            }
            if rules.exclusive_minimum || rules.exclusive_maximum {
                range_attrs.push("exclusive = true".to_string());
            }
            if !range_attrs.is_empty() {
                attrs.push_str(&format!(
                    "    #[validate(range({}))]\n",
                    range_attrs.join(", ")
                ));
            }
        }
        _ if field_type.contains("Vec<") => {
            let mut length_attrs = Vec::new();
            if let Some(min) = rules.min_items {
                length_attrs.push(format!("min = {}", min));
            }
            if let Some(max) = rules.max_items {
                length_attrs.push(format!("max = {}", max));
            }
            if !length_attrs.is_empty() {
                attrs.push_str(&format!(
                    "    #[validate(length({}))]\n",
                    length_attrs.join(", ")
                ));
            }
        }
        _ => {}
    }

    attrs
}

fn generate_model(
    model: &Model,
    required_uses: &mut RequiredUses,
    needs_validator: &mut bool,
    lifetime_types: &HashSet<String>,
) -> Result<String> {
    let mut output = String::new();
    let has_lifetime = lifetime_types.contains(&model.name);

    output.push_str(&generate_description_docs(
        &model.description,
        &model.name,
        "",
    ));

    output.push_str(&generate_custom_attrs(&model.custom_attrs));

    let has_validation = model.fields.iter().any(|f| f.validation_rules.is_some());

    if has_validation {
        *needs_validator = true;
    }

    if !has_custom_derive(&model.custom_attrs) {
        if has_validation {
            output.push_str("#[derive(Debug, Clone, Serialize, Deserialize, Validator)]\n");
        } else {
            output.push_str("#[derive(Debug, Clone, Serialize, Deserialize)]\n");
        }
    }

    if has_lifetime {
        output.push_str(&format!("pub struct {}<'a> {{\n", model.name));
    } else {
        output.push_str(&format!("pub struct {} {{\n", model.name));
    }

    for field in &model.fields {
        let field_type = match field.field_type.as_str() {
            "DateTime" | "DateTime<Utc>" => {
                *required_uses |= RequiredUses::DATETIME;
                "DateTime<Utc>"
            }
            "Date" => {
                *required_uses |= RequiredUses::DATE;
                "NaiveDate"
            }
            "Uuid" => {
                *required_uses |= RequiredUses::UUID;
                "Uuid"
            }
            t if t.contains("RawValue") => {
                *required_uses |= RequiredUses::RAW_VALUE;
                t
            }
            _ => &field.field_type,
        };

        let field_type_str = type_with_lifetime(field_type, lifetime_types);
        let needs_borrow = field_needs_borrow(field_type, lifetime_types);

        let mut lowercased_name = to_snake_case(field.name.as_str());
        if is_reserved_word(&lowercased_name) {
            lowercased_name = format!("r#{lowercased_name}")
        }

        output.push_str(&generate_description_docs(&field.description, "", "    "));

        if let Some(attrs) = &field.custom_attrs {
            for attr in attrs {
                output.push_str(&format!("    {attr}\n"));
            }
        }

        let is_optional = !field.is_required || field.is_nullable;

        let base_type = if field.is_array_ref {
            format!("Vec<{field_type_str}>")
        } else {
            field_type_str.to_string()
        };

        let full_field_type = if is_optional {
            format!("Option<{base_type}>")
        } else {
            base_type
        };

        if let Some(rules) = &field.validation_rules {
            output.push_str(&generate_validator_attrs(rules, &full_field_type));
        }

        if needs_borrow {
            output.push_str("    #[serde(borrow)]\n");
        }

        if lowercased_name != field.name {
            output.push_str(&format!("    #[serde(rename = \"{}\")]\n", field.name));
        }

        if field.should_flatten() && !field_type.contains("RawValue") {
            output.push_str("    #[serde(flatten)]\n");
        }

        output.push_str(&format!("    pub {lowercased_name}: {full_field_type},\n"));
    }

    output.push_str("}\n\n");
    Ok(output)
}

fn generate_request_model(
    request: &RequestModel,
    lifetime_types: &HashSet<String>,
) -> Result<String> {
    let mut output = String::new();
    tracing::info!("Generating request model");
    tracing::info!("{:#?}", request);

    if request.name.is_empty() || request.name == EMPTY_REQUEST_NAME {
        return Ok(String::new());
    }

    let schema_has_lifetime = lifetime_types.contains(&request.schema);
    let schema_type = type_with_lifetime(&request.schema, lifetime_types);

    output.push_str(&format!("/// {}\n", request.name));
    output.push_str("#[derive(Debug, Clone, Serialize)]\n");
    if schema_has_lifetime {
        output.push_str(&format!("pub struct {}<'a> {{\n", request.name));
        output.push_str("    #[serde(borrow)]\n");
    } else {
        output.push_str(&format!("pub struct {} {{\n", request.name));
    }
    output.push_str(&format!("    pub body: {},\n", schema_type));
    output.push_str("}\n");
    Ok(output)
}

fn generate_response_model(
    response: &ResponseModel,
    lifetime_types: &HashSet<String>,
) -> Result<String> {
    if response.name.is_empty() || response.name == EMPTY_RESPONSE_NAME {
        return Ok(String::new());
    }

    let type_name = format!("{}{}", response.name, response.status_code);
    let schema_has_lifetime = lifetime_types.contains(&response.schema);
    let schema_type = type_with_lifetime(&response.schema, lifetime_types);

    let mut output = String::new();

    output.push_str(&generate_description_docs(
        &response.description,
        &type_name,
        "",
    ));

    output.push_str("#[derive(Debug, Clone, Deserialize)]\n");
    if schema_has_lifetime {
        output.push_str(&format!("pub struct {type_name}<'a> {{\n"));
        output.push_str("    #[serde(borrow)]\n");
    } else {
        output.push_str(&format!("pub struct {type_name} {{\n"));
    }
    output.push_str(&format!("    pub body: {},\n", schema_type));
    output.push_str("}\n");

    Ok(output)
}

fn generate_union(union: &UnionModel, lifetime_types: &HashSet<String>) -> Result<String> {
    let mut output = String::new();
    let has_lifetime = lifetime_types.contains(&union.name);

    output.push_str(&format!(
        "/// {} ({})\n",
        union.name,
        match union.union_type {
            UnionType::OneOf => "oneOf",
            UnionType::AnyOf => "anyOf",
        }
    ));
    output.push_str(&generate_custom_attrs(&union.custom_attrs));

    if !has_custom_derive(&union.custom_attrs) {
        output.push_str("#[derive(Debug, Clone, Serialize, Deserialize)]\n");
    }

    if !has_custom_serde(&union.custom_attrs) {
        output.push_str("#[serde(untagged)]\n");
    }

    if has_lifetime {
        output.push_str(&format!("pub enum {}<'a> {{\n", union.name));
    } else {
        output.push_str(&format!("pub enum {} {{\n", union.name));
    }

    for variant in &union.variants {
        match &variant.primitive_type {
            Some(t) => {
                let t_str = type_with_lifetime(t, lifetime_types);
                if field_needs_borrow(t, lifetime_types) {
                    output.push_str(&format!("    #[serde(borrow)]\n"));
                }
                output.push_str(&format!("    {}({}),\n", variant.name, t_str));
            }
            None => {
                let variant_type = type_with_lifetime(&variant.name, lifetime_types);
                if field_needs_borrow(&variant.name, lifetime_types) {
                    output.push_str(&format!("    #[serde(borrow)]\n"));
                }
                output.push_str(&format!("    {}({}),\n", variant.name, variant_type));
            }
        }
    }

    output.push_str("}\n");
    Ok(output)
}

fn generate_composition(
    comp: &CompositionModel,
    required_uses: &mut RequiredUses,
    lifetime_types: &HashSet<String>,
) -> Result<String> {
    let mut output = String::new();
    let has_lifetime = lifetime_types.contains(&comp.name);

    output.push_str(&format!("/// {} (allOf composition)\n", comp.name));
    output.push_str(&generate_custom_attrs(&comp.custom_attrs));

    if !has_custom_derive(&comp.custom_attrs) {
        output.push_str("#[derive(Debug, Clone, Serialize, Deserialize)]\n");
    }

    if has_lifetime {
        output.push_str(&format!("pub struct {}<'a> {{\n", comp.name));
    } else {
        output.push_str(&format!("pub struct {} {{\n", comp.name));
    }

    for field in &comp.all_fields {
        let field_type = match field.field_type.as_str() {
            "f64" => "f64",
            "i64" => "i64",
            "bool" => "bool",
            "DateTime" => {
                *required_uses |= RequiredUses::DATETIME;
                "DateTime<Utc>"
            }
            "Date" => {
                *required_uses |= RequiredUses::DATE;
                "NaiveDate"
            }
            "Uuid" => {
                *required_uses |= RequiredUses::UUID;
                "Uuid"
            }
            t if t.contains("RawValue") => {
                *required_uses |= RequiredUses::RAW_VALUE;
                t
            }
            _ => &field.field_type,
        };

        let field_type_str = type_with_lifetime(field_type, lifetime_types);
        let needs_borrow = field_needs_borrow(field_type, lifetime_types);

        let mut lowercased_name = to_snake_case(field.name.as_str());
        if is_reserved_word(&lowercased_name) {
            lowercased_name = format!("r#{lowercased_name}");
        }

        if needs_borrow {
            output.push_str("    #[serde(borrow)]\n");
        }

        if lowercased_name != field.name {
            output.push_str(&format!("    #[serde(rename = \"{}\")]\n", field.name));
        }

        if let Some(attrs) = &field.custom_attrs {
            for attr in attrs {
                output.push_str(&format!("    {attr}\n"));
            }
        }

        if field.is_array_ref {
            if field.is_required && !field.is_nullable {
                output.push_str(&format!(
                    "    pub {lowercased_name}: Vec<{field_type_str}>,\n",
                ));
            } else {
                output.push_str(&format!(
                    "    pub {lowercased_name}: Option<Vec<{field_type_str}>>,\n",
                ));
            }
        } else if field.is_required && !field.is_nullable {
            output.push_str(&format!(
                "    pub {lowercased_name}: {field_type_str},\n",
            ));
        } else {
            output.push_str(&format!(
                "    pub {lowercased_name}: Option<{field_type_str}>,\n",
            ));
        }
    }

    output.push_str("}\n");
    Ok(output)
}

fn generate_enum(enum_model: &EnumModel) -> Result<String> {
    let mut output = String::new();

    output.push_str(&generate_description_docs(
        &enum_model.description,
        &enum_model.name,
        "",
    ));

    output.push_str(&generate_custom_attrs(&enum_model.custom_attrs));

    // Only add default derive if custom_attrs doesn't already contain a derive
    if !has_custom_derive(&enum_model.custom_attrs) {
        output.push_str("#[derive(Debug, Clone, Serialize, Deserialize)]\n");
    }

    output.push_str(&format!("pub enum {} {{\n", enum_model.name));

    for (i, variant) in enum_model.variants.iter().enumerate() {
        let original = variant.clone();

        let mut rust_name = crate::parser::to_pascal_case(variant);

        let serde_rename = if is_reserved_word(&rust_name) {
            rust_name.push_str("Value");
            Some(original)
        } else if rust_name != original {
            Some(original)
        } else {
            None
        };

        if let Some(rename) = serde_rename {
            output.push_str(&format!("    #[serde(rename = \"{rename}\")]\n"));
        }

        if i + 1 == enum_model.variants.len() {
            output.push_str(&format!("    {rust_name}\n"));
        } else {
            output.push_str(&format!("    {rust_name},\n"));
        }
    }

    output.push_str("}\n");
    Ok(output)
}

fn generate_type_alias(
    type_alias: &TypeAliasModel,
    lifetime_types: &HashSet<String>,
) -> Result<String> {
    let mut output = String::new();
    let has_lifetime = lifetime_types.contains(&type_alias.name);

    output.push_str(&generate_description_docs(
        &type_alias.description,
        &type_alias.name,
        "",
    ));

    output.push_str(&generate_custom_attrs(&type_alias.custom_attrs));
    let target = type_with_lifetime(&type_alias.target_type, lifetime_types);
    if has_lifetime {
        output.push_str(&format!(
            "pub type {}<'a> = {};\n\n",
            type_alias.name, target
        ));
    } else {
        output.push_str(&format!(
            "pub type {} = {};\n\n",
            type_alias.name, target
        ));
    }

    Ok(output)
}

pub fn generate_rust_code(models: &[Model]) -> Result<String> {
    let model_types: Vec<ModelType> = models
        .iter()
        .map(|m| ModelType::Struct(m.clone()))
        .collect();
    let lifetime_types = types_needing_lifetime(&model_types);

    let mut code = create_header();

    code.push_str("use serde::{Serialize, Deserialize};\n");
    code.push_str("use serde_json::value::RawValue;\n");
    code.push_str("use uuid::Uuid;\n");
    code.push_str("use chrono::{DateTime, NaiveDate, Utc};\n\n");

    for model in models {
        let has_lifetime = lifetime_types.contains(&model.name);

        code.push_str(&format!("/// {}\n", model.name));
        code.push_str("#[derive(Debug, Clone, Serialize, Deserialize)]\n");
        if has_lifetime {
            code.push_str(&format!("pub struct {}<'a> {{\n", model.name));
        } else {
            code.push_str(&format!("pub struct {} {{\n", model.name));
        }

        for field in &model.fields {
            let field_type = match field.field_type.as_str() {
                "f64" => "f64",
                "i64" => "i64",
                "bool" => "bool",
                "DateTime" => "DateTime<Utc>",
                "Date" => "NaiveDate",
                "Uuid" => "Uuid",
                _ => &field.field_type,
            };

            let field_type_str = type_with_lifetime(field_type, &lifetime_types);
            let needs_borrow = field_needs_borrow(field_type, &lifetime_types);

            let mut lowercased_name = to_snake_case(field.name.as_str());
            if is_reserved_word(&lowercased_name) {
                lowercased_name = format!("r#{lowercased_name}")
            }

            if needs_borrow {
                code.push_str("    #[serde(borrow)]\n");
            }

            if lowercased_name != field.name {
                code.push_str(&format!("    #[serde(rename = \"{}\")]\n", field.name));
            }

            if field.is_required {
                code.push_str(&format!(
                    "    pub {lowercased_name}: {field_type_str},\n",
                ));
            } else {
                code.push_str(&format!(
                    "    pub {lowercased_name}: Option<{field_type_str}>,\n",
                ));
            }
        }

        code.push_str("}\n\n");
    }

    Ok(code)
}

pub fn generate_lib() -> Result<String> {
    let mut code = create_header();
    code.push_str("pub mod models;\n");

    Ok(code)
}

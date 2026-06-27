// Generates strongly-typed struct/class/interface definitions from a JSON
// subtree. Supports TypeScript, Python (Pydantic v2), Go, Java, C#, Kotlin,
// and Swift as target languages.

use serde_json::Value;

// ─── public API ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CodeLanguage {
    TypeScript,
    Python,
    Go,
    Java,
    CSharp,
    Kotlin,
    Swift,
}

impl CodeLanguage {
    pub fn label(self) -> &'static str {
        match self {
            CodeLanguage::TypeScript => "TypeScript Interface",
            CodeLanguage::Python     => "Python (Pydantic v2)",
            CodeLanguage::Go         => "Go Struct",
            CodeLanguage::Java       => "Java POJO",
            CodeLanguage::CSharp     => "C# Class",
            CodeLanguage::Kotlin     => "Kotlin Data Class",
            CodeLanguage::Swift      => "Swift Codable Struct",
        }
    }
}

pub const LANGUAGES: &[CodeLanguage] = &[
    CodeLanguage::TypeScript,
    CodeLanguage::Python,
    CodeLanguage::Go,
    CodeLanguage::Java,
    CodeLanguage::CSharp,
    CodeLanguage::Kotlin,
    CodeLanguage::Swift,
];

/// Generate a code snippet from raw JSON bytes for the given language.
pub fn generate(json_bytes: &[u8], language: CodeLanguage, root_name: &str) -> String {
    let value: Value = match serde_json::from_slice(json_bytes) {
        Ok(v)  => v,
        Err(e) => return format!("// Error parsing JSON: {e}"),
    };
    let mut collector = SchemaCollector::default();
    let root_type = collector.collect(&value, root_name);
    if collector.schemas.is_empty() {
        return format!(
            "// The selected value is a {}, not an object or array of objects.",
            primitive_kind(&root_type)
        );
    }
    match language {
        CodeLanguage::TypeScript => emit_typescript(&collector.schemas),
        CodeLanguage::Python     => emit_python(&collector.schemas),
        CodeLanguage::Go         => emit_go(&collector.schemas),
        CodeLanguage::Java       => emit_java(&collector.schemas),
        CodeLanguage::CSharp     => emit_csharp(&collector.schemas),
        CodeLanguage::Kotlin     => emit_kotlin(&collector.schemas),
        CodeLanguage::Swift      => emit_swift(&collector.schemas),
    }
}

/// Convert a JSON key to PascalCase (usable as a class/struct/type name).
/// This is also used from main.rs to derive the root name from the node key.
pub fn to_pascal_case(s: &str) -> String {
    let mut out = String::new();
    let mut cap_next = true;
    for c in s.chars() {
        if c == '_' || c == '-' || c == ' ' || c == '.' || c == ':' {
            cap_next = true;
        } else if !c.is_alphanumeric() {
            cap_next = true; // drop special chars, capitalize what follows
        } else if cap_next {
            out.extend(c.to_uppercase());
            cap_next = false;
        } else {
            out.push(c);
        }
    }
    // Guard against empty or digit-leading results
    if out.is_empty() {
        return "Field".to_owned();
    }
    if out.chars().next().map_or(false, |c| c.is_ascii_digit()) {
        out.insert_str(0, "Type");
    }
    out
}

// ─── internals ───────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
enum InferredType {
    Str,
    Int,
    Float,
    Bool,
    Object(String),
    Array(Box<InferredType>),
    Any, // covers Null and genuinely unknown element types
}

struct FieldDef {
    original_key: String,
    ty:           InferredType,
    nullable:     bool,
}

struct ObjectSchema {
    name:   String,
    fields: Vec<FieldDef>,
}

#[derive(Default)]
struct SchemaCollector {
    /// Accumulated in bottom-up order: nested types appear before the types
    /// that reference them, which is the correct emission order.
    schemas: Vec<ObjectSchema>,
}

impl SchemaCollector {
    fn collect(&mut self, value: &Value, name: &str) -> InferredType {
        match value {
            Value::Object(map) => {
                let unique = self.unique_name(name);
                let mut fields = Vec::new();
                for (key, val) in map {
                    let child_name = to_pascal_case(key);
                    let (ty, nullable) = self.infer_field(val, &child_name);
                    fields.push(FieldDef { original_key: key.clone(), ty, nullable });
                }
                self.schemas.push(ObjectSchema { name: unique.clone(), fields });
                InferredType::Object(unique)
            }
            Value::Array(arr) => {
                InferredType::Array(Box::new(self.infer_array_elem(arr, name)))
            }
            Value::String(_) => InferredType::Str,
            Value::Number(n) => {
                // serde_json stores JSON floats (those with . or e/E) as N::Float
                // internally; is_f64() is true *only* for that variant.
                if n.is_f64() { InferredType::Float } else { InferredType::Int }
            }
            Value::Bool(_) => InferredType::Bool,
            Value::Null    => InferredType::Any,
        }
    }

    fn infer_field(&mut self, val: &Value, child_name: &str) -> (InferredType, bool) {
        if let Value::Null = val {
            return (InferredType::Any, true); // null → optional/unknown
        }
        let ty = self.collect(val, child_name);
        (ty, false)
    }

    fn infer_array_elem(&mut self, arr: &[Value], base_name: &str) -> InferredType {
        if arr.is_empty() {
            return InferredType::Any;
        }
        // Collect object maps for merging; fall back to first element otherwise.
        let obj_maps: Vec<&serde_json::Map<String, Value>> = arr
            .iter()
            .filter_map(|v| if let Value::Object(m) = v { Some(m) } else { None })
            .collect();

        if obj_maps.is_empty() {
            self.collect(&arr[0], base_name)
        } else {
            self.merge_object_schemas(obj_maps, base_name)
        }
    }

    /// Union of keys from all maps; a key absent from any element is nullable.
    fn merge_object_schemas(
        &mut self,
        maps: Vec<&serde_json::Map<String, Value>>,
        name: &str,
    ) -> InferredType {
        let mut all_keys: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for m in &maps {
            for k in m.keys() {
                if seen.insert(k.clone()) {
                    all_keys.push(k.clone());
                }
            }
        }
        let unique = self.unique_name(name);
        let mut fields = Vec::new();
        for key in all_keys {
            let child_name = to_pascal_case(&key);
            let in_all = maps.iter().all(|m| m.contains_key(&key));
            let sample = maps
                .iter()
                .find_map(|m| m.get(&key).filter(|v| !matches!(v, Value::Null)))
                .or_else(|| maps.iter().find_map(|m| m.get(&key)));
            let (ty, null_from_val) = match sample {
                Some(v) => self.infer_field(v, &child_name),
                None    => (InferredType::Any, true),
            };
            fields.push(FieldDef { original_key: key, ty, nullable: !in_all || null_from_val });
        }
        self.schemas.push(ObjectSchema { name: unique.clone(), fields });
        InferredType::Object(unique)
    }

    fn unique_name(&self, base: &str) -> String {
        if !self.schemas.iter().any(|s| s.name == base) {
            return base.to_owned();
        }
        let mut i = 2usize;
        loop {
            let candidate = format!("{base}{i}");
            if !self.schemas.iter().any(|s| s.name == candidate) {
                return candidate;
            }
            i += 1;
        }
    }
}

fn primitive_kind(ty: &InferredType) -> &'static str {
    match ty {
        InferredType::Str     => "string",
        InferredType::Int     => "integer",
        InferredType::Float   => "float",
        InferredType::Bool    => "boolean",
        InferredType::Array(_)=> "array of primitives",
        InferredType::Object(_) => "object",
        InferredType::Any     => "null/unknown",
    }
}

// ─── naming helpers ───────────────────────────────────────────────────────────

fn to_camel_case(s: &str) -> String {
    let pascal = to_pascal_case(s);
    let mut chars = pascal.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
        None        => String::new(),
    }
}

fn to_snake_case(s: &str) -> String {
    let mut out = String::new();
    let mut prev_upper = false;
    for c in s.chars() {
        if c == '-' || c == ' ' || c == '.' || c == ':' || c == '_' {
            if !out.ends_with('_') { out.push('_'); }
            prev_upper = false;
        } else if !c.is_alphanumeric() {
            prev_upper = false; // skip special chars
        } else if c.is_uppercase() {
            if !out.is_empty() && !prev_upper && !out.ends_with('_') {
                out.push('_');
            }
            out.extend(c.to_lowercase());
            prev_upper = true;
        } else {
            out.push(c);
            prev_upper = false;
        }
    }
    let out = out.trim_matches('_');
    // Collapse consecutive underscores
    let mut result = String::new();
    let mut last_under = false;
    for c in out.chars() {
        if c == '_' {
            if !last_under { result.push('_'); }
            last_under = true;
        } else {
            result.push(c);
            last_under = false;
        }
    }
    if result.is_empty() { "field".to_owned() } else { result }
}

// ─── TypeScript ───────────────────────────────────────────────────────────────

fn ts_type(ty: &InferredType, nullable: bool) -> String {
    let base = match ty {
        InferredType::Str       => "string".to_owned(),
        InferredType::Int
        | InferredType::Float   => "number".to_owned(),
        InferredType::Bool      => "boolean".to_owned(),
        InferredType::Object(n) => n.clone(),
        InferredType::Array(el) => format!("{}[]", ts_type(el, false)),
        InferredType::Any       => "unknown".to_owned(),
    };
    if nullable { format!("{base} | null") } else { base }
}

fn emit_typescript(schemas: &[ObjectSchema]) -> String {
    let mut out = String::new();
    for schema in schemas {
        out.push_str(&format!("export interface {} {{\n", schema.name));
        for f in &schema.fields {
            let name = to_camel_case(&f.original_key);
            let opt  = if f.nullable { "?" } else { "" };
            let ty   = ts_type(&f.ty, f.nullable && !matches!(f.ty, InferredType::Any));
            out.push_str(&format!("  {name}{opt}: {ty};\n"));
        }
        out.push_str("}\n\n");
    }
    out.trim_end().to_owned()
}

// ─── Python / Pydantic v2 ─────────────────────────────────────────────────────

fn py_type(ty: &InferredType, nullable: bool) -> String {
    let base = match ty {
        InferredType::Str       => "str".to_owned(),
        InferredType::Int       => "int".to_owned(),
        InferredType::Float     => "float".to_owned(),
        InferredType::Bool      => "bool".to_owned(),
        InferredType::Object(n) => n.clone(),
        InferredType::Array(el) => format!("List[{}]", py_type(el, false)),
        InferredType::Any       => "Any".to_owned(),
    };
    if nullable { format!("Optional[{base}]") } else { base }
}

fn emit_python(schemas: &[ObjectSchema]) -> String {
    let mut out = String::from(
        "from __future__ import annotations\nfrom pydantic import BaseModel, Field\nfrom typing import Any, List, Optional\n\n\n",
    );
    for schema in schemas {
        out.push_str(&format!("class {}(BaseModel):\n", schema.name));
        if schema.fields.is_empty() {
            out.push_str("    pass\n");
        }
        for f in &schema.fields {
            let name     = to_snake_case(&f.original_key);
            let ty       = py_type(&f.ty, f.nullable);
            let has_alias = name != f.original_key;
            match (f.nullable, has_alias) {
                (true,  true)  => out.push_str(&format!(
                    "    {name}: {ty} = Field(None, alias=\"{}\")\n", f.original_key
                )),
                (true,  false) => out.push_str(&format!("    {name}: {ty} = None\n")),
                (false, true)  => out.push_str(&format!(
                    "    {name}: {ty} = Field(alias=\"{}\")\n", f.original_key
                )),
                (false, false) => out.push_str(&format!("    {name}: {ty}\n")),
            }
        }
        out.push_str("\n\n");
    }
    out.trim_end().to_owned()
}

// ─── Go ───────────────────────────────────────────────────────────────────────

fn go_type(ty: &InferredType, nullable: bool) -> String {
    match ty {
        InferredType::Str       => if nullable { "*string"   } else { "string"   }.to_owned(),
        InferredType::Int       => if nullable { "*int64"    } else { "int64"    }.to_owned(),
        InferredType::Float     => if nullable { "*float64"  } else { "float64"  }.to_owned(),
        InferredType::Bool      => if nullable { "*bool"     } else { "bool"     }.to_owned(),
        InferredType::Object(n) => if nullable { format!("*{n}") } else { n.clone() },
        InferredType::Array(el) => format!("[]{}", go_type(el, false)),
        InferredType::Any       => "interface{}".to_owned(),
    }
}

fn emit_go(schemas: &[ObjectSchema]) -> String {
    let mut out = String::from("package main\n\n");
    for schema in schemas {
        out.push_str(&format!("type {} struct {{\n", schema.name));
        for f in &schema.fields {
            let name  = to_pascal_case(&f.original_key);
            let ty    = go_type(&f.ty, f.nullable);
            let omit  = if f.nullable { ",omitempty" } else { "" };
            out.push_str(&format!("\t{name} {ty} `json:\"{}{omit}\"`\n", f.original_key));
        }
        out.push_str("}\n\n");
    }
    out.trim_end().to_owned()
}

// ─── Java ─────────────────────────────────────────────────────────────────────

fn java_type(ty: &InferredType) -> String {
    match ty {
        InferredType::Str       => "String".to_owned(),
        InferredType::Int       => "Long".to_owned(),
        InferredType::Float     => "Double".to_owned(),
        InferredType::Bool      => "Boolean".to_owned(),
        InferredType::Object(n) => n.clone(),
        InferredType::Array(el) => format!("List<{}>", java_type(el)),
        InferredType::Any       => "Object".to_owned(),
    }
}

fn emit_java(schemas: &[ObjectSchema]) -> String {
    let mut out = String::from(
        "import com.fasterxml.jackson.annotation.JsonProperty;\nimport java.util.List;\n\n",
    );
    for schema in schemas {
        out.push_str(&format!("public class {} {{\n", schema.name));
        for f in &schema.fields {
            let name  = to_camel_case(&f.original_key);
            let ty    = java_type(&f.ty);
            out.push_str(&format!("    @JsonProperty(\"{}\")\n", f.original_key));
            out.push_str(&format!("    private {ty} {name};\n\n"));
        }
        // Remove the trailing blank line inside the closing brace
        if out.ends_with("\n\n") { out.pop(); }
        out.push_str("}\n\n");
    }
    out.trim_end().to_owned()
}

// ─── C# ───────────────────────────────────────────────────────────────────────

fn csharp_type(ty: &InferredType, nullable: bool) -> String {
    match ty {
        InferredType::Str       => if nullable { "string?"  } else { "string"  }.to_owned(),
        InferredType::Int       => if nullable { "long?"    } else { "long"    }.to_owned(),
        InferredType::Float     => if nullable { "double?"  } else { "double"  }.to_owned(),
        InferredType::Bool      => if nullable { "bool?"    } else { "bool"    }.to_owned(),
        InferredType::Object(n) => if nullable { format!("{n}?") } else { n.clone() },
        InferredType::Array(el) => format!("List<{}>", csharp_type(el, false)),
        InferredType::Any       => "object?".to_owned(),
    }
}

fn emit_csharp(schemas: &[ObjectSchema]) -> String {
    let mut out = String::from(
        "using System.Collections.Generic;\nusing System.Text.Json.Serialization;\n\n",
    );
    for schema in schemas {
        out.push_str(&format!("public class {}\n{{\n", schema.name));
        for f in &schema.fields {
            let name = to_pascal_case(&f.original_key);
            let ty   = csharp_type(&f.ty, f.nullable);
            out.push_str(&format!("    [JsonPropertyName(\"{}\")]\n", f.original_key));
            out.push_str(&format!("    public {ty} {name} {{ get; set; }}\n\n"));
        }
        if out.ends_with("\n\n") { out.pop(); }
        out.push_str("}\n\n");
    }
    out.trim_end().to_owned()
}

// ─── Kotlin ───────────────────────────────────────────────────────────────────

fn kotlin_type(ty: &InferredType, nullable: bool) -> String {
    let base = match ty {
        InferredType::Str       => "String".to_owned(),
        InferredType::Int       => "Long".to_owned(),
        InferredType::Float     => "Double".to_owned(),
        InferredType::Bool      => "Boolean".to_owned(),
        InferredType::Object(n) => n.clone(),
        InferredType::Array(el) => format!("List<{}>", kotlin_type(el, false)),
        InferredType::Any       => "Any".to_owned(),
    };
    if nullable { format!("{base}?") } else { base }
}

fn emit_kotlin(schemas: &[ObjectSchema]) -> String {
    let mut out = String::from(
        "import kotlinx.serialization.SerialName\nimport kotlinx.serialization.Serializable\n\n",
    );
    for schema in schemas {
        out.push_str("@Serializable\n");
        out.push_str(&format!("data class {}(\n", schema.name));
        let n = schema.fields.len();
        for (i, f) in schema.fields.iter().enumerate() {
            let name    = to_camel_case(&f.original_key);
            let ty      = kotlin_type(&f.ty, f.nullable);
            let default = if f.nullable { " = null" } else { "" };
            let comma   = if i + 1 < n { "," } else { "" };
            out.push_str(&format!(
                "    @SerialName(\"{}\") val {name}: {ty}{default}{comma}\n",
                f.original_key
            ));
        }
        out.push_str(")\n\n");
    }
    out.trim_end().to_owned()
}

// ─── Swift ────────────────────────────────────────────────────────────────────

fn swift_type(ty: &InferredType, nullable: bool) -> String {
    let base = match ty {
        InferredType::Str       => "String".to_owned(),
        InferredType::Int       => "Int".to_owned(),
        InferredType::Float     => "Double".to_owned(),
        InferredType::Bool      => "Bool".to_owned(),
        InferredType::Object(n) => n.clone(),
        InferredType::Array(el) => format!("[{}]", swift_type(el, false)),
        InferredType::Any       => "AnyCodable".to_owned(),
    };
    if nullable { format!("{base}?") } else { base }
}

fn emit_swift(schemas: &[ObjectSchema]) -> String {
    let any_used = schemas
        .iter()
        .flat_map(|s| s.fields.iter())
        .any(|f| matches!(f.ty, InferredType::Any));

    let mut out = String::from("import Foundation\n");
    if any_used {
        // AnyCodable is from https://github.com/Flight-School/AnyCodable
        out.push_str("// AnyCodable: https://github.com/Flight-School/AnyCodable\n");
    }
    out.push('\n');

    for schema in schemas {
        out.push_str(&format!("struct {}: Codable {{\n", schema.name));
        for f in &schema.fields {
            let name = to_camel_case(&f.original_key);
            let ty   = swift_type(&f.ty, f.nullable);
            out.push_str(&format!("    let {name}: {ty}\n"));
        }
        // Only emit CodingKeys when at least one Swift field name differs from its JSON key
        let needs_coding_keys = schema
            .fields
            .iter()
            .any(|f| to_camel_case(&f.original_key) != f.original_key);
        if needs_coding_keys {
            out.push('\n');
            out.push_str("    enum CodingKeys: String, CodingKey {\n");
            for f in &schema.fields {
                let name = to_camel_case(&f.original_key);
                if name != f.original_key {
                    out.push_str(&format!("        case {name} = \"{}\"\n", f.original_key));
                } else {
                    out.push_str(&format!("        case {name}\n"));
                }
            }
            out.push_str("    }\n");
        }
        out.push_str("}\n\n");
    }
    out.trim_end().to_owned()
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = br#"{
        "user_id": 42,
        "first-name": "Alice",
        "score": 9.5,
        "active": true,
        "tags": ["rust", "json"],
        "address": { "city": "Tel Aviv", "zip": null }
    }"#;

    #[test]
    fn typescript_compiles() {
        let code = generate(SAMPLE, CodeLanguage::TypeScript, "User");
        assert!(code.contains("export interface User"));
        assert!(code.contains("userId: number"));
        assert!(code.contains("score: number"));
        assert!(code.contains("active: boolean"));
        assert!(code.contains("tags: string[]"));
        assert!(code.contains("address: Address"));
    }

    #[test]
    fn python_compiles() {
        let code = generate(SAMPLE, CodeLanguage::Python, "User");
        assert!(code.contains("class User(BaseModel)"));
        assert!(code.contains("user_id: int"));
        assert!(code.contains("score: float"));
    }

    #[test]
    fn go_compiles() {
        let code = generate(SAMPLE, CodeLanguage::Go, "User");
        assert!(code.contains("type User struct"));
        assert!(code.contains("UserId int64"));
        assert!(code.contains(r#"json:"user_id""#));
    }

    #[test]
    fn naming_helpers() {
        assert_eq!(to_pascal_case("first-name"), "FirstName");
        assert_eq!(to_pascal_case("user_id"),    "UserId");
        assert_eq!(to_camel_case("first-name"),  "firstName");
        assert_eq!(to_snake_case("firstName"),   "first_name");
        assert_eq!(to_snake_case("first-name"),  "first_name");
    }
}

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

pub fn generate(csv_dir: &Path, out_dir: &Path) -> Result<(), String> {
    let mut tables = collect_csv_tables(csv_dir)?;
    tables.sort_by(|left, right| left.file_stem.cmp(&right.file_stem));

    fs::create_dir_all(out_dir)
        .map_err(|error| format!("failed to create csv code dir {}: {error}", out_dir.display()))?;

    let mut module_names = Vec::new();
    for table in &tables {
        let module_name = table.module_name.clone();
        let generated = render_table_module(table);
        let out_file = out_dir.join(format!("{module_name}.rs"));
        write_if_changed(&out_file, &generated).map_err(|error| {
            format!(
                "failed to write generated csv table file {}: {error}",
                out_file.display()
            )
        })?;
        module_names.push(module_name);
    }

    let mod_file = out_dir.join("mod.rs");
    let mod_contents = render_mod_rs(&module_names);
    write_if_changed(&mod_file, &mod_contents).map_err(|error| {
        format!(
            "failed to write generated csv module file {}: {error}",
            mod_file.display()
        )
    })?;

    Ok(())
}

#[derive(Clone, Debug)]
struct CsvTable {
    module_name: String,
    file_stem: String,
    schema_const_name: String,
    row_struct_name: String,
    table_struct_name: String,
    fields: Vec<CsvField>,
    id_field: CsvField,
    secondary_indexes: Vec<CsvField>,
    schema_signature: String,
    has_string_pool: bool,
}

#[derive(Clone, Debug)]
struct CsvField {
    original_name: String,
    rust_name: String,
    csv_type: CsvType,
}

#[derive(Clone, Debug)]
enum CsvType {
    Int,
    Int64,
    Float,
    String,
    Array(ScalarType),
    Dict(ScalarType, ScalarType),
}

#[derive(Clone, Copy, Debug)]
enum ScalarType {
    Int,
    Int64,
    Float,
    String,
}

fn collect_csv_tables(csv_dir: &Path) -> Result<Vec<CsvTable>, String> {
    let entries = fs::read_dir(csv_dir)
        .map_err(|error| format!("failed to read csv dir {}: {error}", csv_dir.display()))?;
    let indexed_columns = builtin_indexed_columns();
    let mut tables = Vec::new();

    for entry in entries {
        let entry = entry.map_err(|error| format!("failed to read csv dir entry: {error}"))?;
        let path = entry.path();

        if path.extension().and_then(|ext| ext.to_str()) != Some("csv") {
            continue;
        }

        let contents = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read csv file {}: {error}", path.display()))?;
        let mut lines = contents.lines();
        let header_line = lines
            .next()
            .ok_or_else(|| format!("csv file {} is missing header line", path.display()))?;
        let type_line = lines
            .next()
            .ok_or_else(|| format!("csv file {} is missing type line", path.display()))?;

        let headers = parse_csv_line(header_line);
        let types = parse_csv_line(type_line);

        if headers.len() != types.len() {
            return Err(format!(
                "csv file {} has mismatched header/type column count: {} vs {}",
                path.display(),
                headers.len(),
                types.len()
            ));
        }

        if headers.is_empty() {
            return Err(format!("csv file {} has no columns", path.display()));
        }

        if headers[0] != "Id" {
            return Err(format!(
                "csv file {} must use `Id` as the first column name",
                path.display()
            ));
        }

        let file_stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| format!("invalid csv file name {}", path.display()))?
            .to_string();
        let module_name = to_snake_case(&file_stem);
        let schema_const_name = format!("{}_SCHEMA_SIGNATURE", to_upper_snake_case(&file_stem));
        let row_struct_name = format!("{}Row", to_pascal_case(&file_stem));
        let table_struct_name = to_pascal_case(&file_stem);

        let mut used_field_names = BTreeSet::new();
        let mut fields = Vec::with_capacity(headers.len());

        for (header, type_name) in headers.iter().zip(types.iter()) {
            if header.is_empty() {
                return Err(format!("csv file {} contains an empty field name", path.display()));
            }

            if !used_field_names.insert(header.clone()) {
                return Err(format!(
                    "csv file {} contains duplicate field name `{header}`",
                    path.display()
                ));
            }

            let csv_type = parse_csv_type(type_name)
                .map_err(|error| format!("csv file {} field `{header}`: {error}", path.display()))?;

            fields.push(CsvField {
                original_name: header.clone(),
                rust_name: to_snake_case(header),
                csv_type,
            });
        }

        let id_field = fields[0].clone();
        match id_field.csv_type {
            CsvType::Int | CsvType::Int64 => {}
            _ => {
                return Err(format!(
                    "csv file {} requires `Id` to use `int` or `int64`",
                    path.display()
                ));
            }
        }

        let mut secondary_indexes = Vec::new();
        if let Some(indexed_names) = indexed_columns.get(file_stem.as_str()) {
            for indexed_name in indexed_names {
                let field = fields
                    .iter()
                    .find(|field| field.original_name == *indexed_name)
                    .ok_or_else(|| {
                        format!(
                            "csv file {} configured index column `{indexed_name}` does not exist",
                            path.display()
                        )
                    })?
                    .clone();

                if field.original_name == "Id" {
                    continue;
                }

                match field.csv_type {
                    CsvType::Int | CsvType::Int64 | CsvType::String => {
                        secondary_indexes.push(field);
                    }
                    _ => {
                        return Err(format!(
                            "csv file {} field `{indexed_name}` uses unsupported index type",
                            path.display()
                        ));
                    }
                }
            }
        }

        let schema_signature = headers
            .iter()
            .zip(types.iter())
            .map(|(header, ty)| format!("{header}:{ty}"))
            .collect::<Vec<_>>()
            .join("|");
        let has_string_pool = fields.iter().any(|field| field.csv_type.contains_string());

        tables.push(CsvTable {
            file_stem,
            module_name,
            schema_const_name,
            row_struct_name,
            table_struct_name,
            fields,
            id_field,
            secondary_indexes,
            schema_signature,
            has_string_pool,
        });
    }

    Ok(tables)
}

fn parse_csv_line(line: &str) -> Vec<String> {
    let mut columns = Vec::new();
    let mut current = String::new();
    let mut generic_depth = 0u32;

    for ch in line.chars() {
        match ch {
            '<' => {
                generic_depth += 1;
                current.push(ch);
            }
            '>' => {
                generic_depth = generic_depth.saturating_sub(1);
                current.push(ch);
            }
            ',' if generic_depth == 0 => {
                columns.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    columns.push(current.trim().to_string());
    if let Some(first) = columns.first_mut() {
        *first = first.trim_start_matches('﻿').to_string();
    }
    columns
}

fn parse_csv_type(type_name: &str) -> Result<CsvType, String> {
    match type_name {
        "int" => return Ok(CsvType::Int),
        "int64" => return Ok(CsvType::Int64),
        "float" => return Ok(CsvType::Float),
        "string" => return Ok(CsvType::String),
        _ => {}
    }

    if let Some(inner) = strip_wrapped(type_name, "Array<", '>') {
        return Ok(CsvType::Array(parse_scalar_type(inner)?));
    }

    if let Some(inner) = strip_wrapped(type_name, "Dict<", '>') {
        let parts = inner.split(',').map(|part| part.trim()).collect::<Vec<_>>();
        if parts.len() != 2 {
            return Err(format!("unsupported dict type syntax `{type_name}`"));
        }
        return Ok(CsvType::Dict(
            parse_scalar_type(parts[0])?,
            parse_scalar_type(parts[1])?,
        ));
    }

    Err(format!("unsupported csv type `{type_name}`"))
}

fn parse_scalar_type(type_name: &str) -> Result<ScalarType, String> {
    match type_name {
        "int" => Ok(ScalarType::Int),
        "int64" => Ok(ScalarType::Int64),
        "float" => Ok(ScalarType::Float),
        "string" => Ok(ScalarType::String),
        _ => Err(format!("unsupported scalar type `{type_name}`")),
    }
}

fn strip_wrapped<'a>(value: &'a str, prefix: &str, suffix: char) -> Option<&'a str> {
    value.strip_prefix(prefix)?.strip_suffix(suffix)
}

impl CsvType {
    fn rust_type(&self) -> String {
        match self {
            Self::Int => "i32".to_string(),
            Self::Int64 => "i64".to_string(),
            Self::Float => "f32".to_string(),
            Self::String => "StringKey".to_string(),
            Self::Array(inner) => format!("Vec<{}>", inner.rust_value_type()),
            Self::Dict(key, value) => format!(
                "std::collections::HashMap<{}, {}>",
                key.rust_value_type(),
                value.rust_value_type()
            ),
        }
    }

    fn index_key_type(&self) -> Option<String> {
        match self {
            Self::Int => Some("i32".to_string()),
            Self::Int64 => Some("i64".to_string()),
            Self::String => Some("StringKey".to_string()),
            _ => None,
        }
    }

    fn contains_string(&self) -> bool {
        match self {
            Self::String => true,
            Self::Array(inner) => matches!(inner, ScalarType::String),
            Self::Dict(key, value) => {
                matches!(key, ScalarType::String) || matches!(value, ScalarType::String)
            }
            _ => false,
        }
    }
}

impl ScalarType {
    fn rust_value_type(self) -> &'static str {
        match self {
            Self::Int => "i32",
            Self::Int64 => "i64",
            Self::Float => "f32",
            Self::String => "StringKey",
        }
    }
}

impl CsvField {
    fn loader_expr(&self, index: usize) -> String {
        match &self.csv_type {
            CsvType::Int => format!("reader.parse_i32({}, {:?})?", index, self.original_name),
            CsvType::Int64 => format!("reader.parse_i64({}, {:?})?", index, self.original_name),
            CsvType::Float => format!("reader.parse_f32({}, {:?})?", index, self.original_name),
            CsvType::String => format!("reader.parse_string_key({}, {:?}, &mut string_pool)?", index, self.original_name),
            CsvType::Array(ScalarType::String) => format!("reader.parse_string_array({}, {:?}, &mut string_pool)?", index, self.original_name),
            CsvType::Dict(ScalarType::String, ScalarType::Int) => format!("reader.parse_string_int_dict({}, {:?}, &mut string_pool)?", index, self.original_name),
            _ => format!("unimplemented!(\"loader for field {}\")", self.original_name),
        }
    }

    fn index_insert_stmt(&self) -> Option<String> {
        self.csv_type.index_key_type().map(|_| {
            format!(
                "            table.by_{}.entry(row.{}).or_default().push(table.rows.len());
",
                self.rust_name, self.rust_name
            )
        })
    }
}

fn render_table_module(table: &CsvTable) -> String {
    let mut code = String::new();
    code.push_str("// @generated by apps/game-server/tools/csv_codegen.rs\n");
    code.push_str("// This file is generated at compile time. Do not edit manually.\n\n");
    render_table(table, &mut code);
    code
}

fn render_mod_rs(module_names: &[String]) -> String {
    let mut code = String::new();
    code.push_str("// @generated by apps/game-server/tools/csv_codegen.rs\n");
    code.push_str("// This file is generated at compile time. Do not edit manually.\n\n");
    for module_name in module_names {
        code.push_str(&format!("pub mod {};\n", module_name));
    }
    code
}

fn render_table(table: &CsvTable, out: &mut String) {
    if table.has_string_pool {
        out.push_str(
            "use crate::config_table::{CsvLoadError, CsvRowReader, CsvTableLoader, StringPoolBuilder};\n\n",
        );
        out.push_str("pub type StringKey = u32;\n\n");
    } else {
        out.push_str("use crate::config_table::{CsvLoadError, CsvRowReader, CsvTableLoader};\n\n");
    }

    out.push_str(&format!(
        "pub const {}: &str = {:?};\n\n",
        table.schema_const_name, table.schema_signature
    ));

    out.push_str("#[derive(Debug, Clone, Default)]\n");
    out.push_str(&format!("pub struct {} {{\n", table.row_struct_name));
    for field in &table.fields {
        out.push_str(&format!(
            "    pub {}: {},\n",
            field.rust_name,
            field.csv_type.rust_type()
        ));
    }
    out.push_str("}\n\n");

    out.push_str("#[derive(Debug, Clone, Default)]\n");
    out.push_str(&format!("pub struct {} {{\n", table.table_struct_name));
    if table.has_string_pool {
        out.push_str("    pub string_pool: std::collections::HashMap<StringKey, String>,\n");
    }
    out.push_str(&format!("    pub rows: Vec<{}>,\n", table.row_struct_name));
    out.push_str(&format!(
        "    pub by_id: std::collections::HashMap<{}, usize>,\n",
        table.id_field.csv_type.rust_type()
    ));
    for field in &table.secondary_indexes {
        if let Some(index_key_type) = field.csv_type.index_key_type() {
            out.push_str(&format!(
                "    pub by_{}: std::collections::HashMap<{}, Vec<usize>>,\n",
                field.rust_name, index_key_type
            ));
        }
    }
    out.push_str("}\n\n");

    out.push_str(&format!("impl CsvTableLoader for {} {{\n", table.table_struct_name));
    out.push_str(&format!(
        "    const TABLE_NAME: &'static str = {:?};\n",
        table.file_stem
    ));
    out.push_str(&format!(
        "    const SCHEMA_SIGNATURE: &'static str = {};\n\n",
        table.schema_const_name
    ));
    out.push_str("    fn load_from_csv(path: &std::path::Path) -> Result<Self, CsvLoadError> {\n");
    out.push_str("        let contents = std::fs::read_to_string(path)?;\n");
    out.push_str("        let mut lines = contents.lines();\n");
    out.push_str("        let header_line = lines.next().ok_or_else(|| CsvLoadError::InvalidSchema(format!(\"table {} missing header line\", Self::TABLE_NAME)))?;\n");
    out.push_str("        let type_line = lines.next().ok_or_else(|| CsvLoadError::InvalidSchema(format!(\"table {} missing type line\", Self::TABLE_NAME)))?;\n");
    out.push_str("        let header_columns = crate::config_table::parse_csv_columns(header_line);\n");
    out.push_str("        let type_columns = crate::config_table::parse_csv_columns(type_line);\n");
    out.push_str("        let signature = crate::config_table::schema_signature(&header_columns, &type_columns);\n");
    out.push_str("        if signature != Self::SCHEMA_SIGNATURE {\n");
    out.push_str("            return Err(CsvLoadError::InvalidSchema(format!(\"table {} schema mismatch: expected {}, got {}\", Self::TABLE_NAME, Self::SCHEMA_SIGNATURE, signature)));\n");
    out.push_str("        }\n\n");
    out.push_str("        let mut table = Self::default();\n");
    if table.has_string_pool {
        out.push_str("        let mut string_pool = StringPoolBuilder::default();\n");
    }
    out.push_str("\n");
    out.push_str("        for (row_offset, line) in lines.enumerate() {\n");
    out.push_str("            let trimmed = line.trim();\n");
    out.push_str("            if trimmed.is_empty() || trimmed.starts_with('#') {\n");
    out.push_str("                continue;\n");
    out.push_str("            }\n");
    out.push_str("            let columns = crate::config_table::parse_csv_columns(trimmed);\n");
    out.push_str("            if columns.len() != header_columns.len() {\n");
    out.push_str("                return Err(CsvLoadError::InvalidRow(format!(\"table {} row {} column count mismatch: expected {}, got {}\", Self::TABLE_NAME, row_offset + 3, header_columns.len(), columns.len())));\n");
    out.push_str("            }\n");
    out.push_str("            let reader = CsvRowReader::new(Self::TABLE_NAME, row_offset + 3, &columns);\n");
    out.push_str(&format!("            let row = {} {{\n", table.row_struct_name));
    for (index, field) in table.fields.iter().enumerate() {
        out.push_str(&format!(
            "                {}: {},\n",
            field.rust_name,
            field.loader_expr(index)
        ));
    }
    out.push_str("            };\n\n");
    out.push_str("            if table.by_id.insert(row.id, table.rows.len()).is_some() {\n");
    out.push_str("                return Err(CsvLoadError::InvalidRow(format!(\"table {} row {} duplicate id {}\", Self::TABLE_NAME, row_offset + 3, row.id)));\n");
    out.push_str("            }\n");
    for field in &table.secondary_indexes {
        if let Some(stmt) = field.index_insert_stmt() {
            out.push_str(&stmt);
        }
    }
    out.push_str("            table.rows.push(row);\n");
    out.push_str("        }\n\n");
    if table.has_string_pool {
        out.push_str("        table.string_pool = string_pool.finish();\n");
    }
    out.push_str("        Ok(table)\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");

    out.push_str(&format!("impl {} {{\n", table.table_struct_name));
    out.push_str(&format!(
        "    pub fn get(&self, id: {}) -> Option<&{}> {{\n",
        table.id_field.csv_type.rust_type(),
        table.row_struct_name
    ));
    out.push_str("        self.by_id\n");
    out.push_str("            .get(&id)\n");
    out.push_str("            .and_then(|&row_index| self.rows.get(row_index))\n");
    out.push_str("    }\n\n");
    out.push_str(&format!("    pub fn all(&self) -> &[{}] {{\n", table.row_struct_name));
    out.push_str("        &self.rows\n");
    out.push_str("    }\n\n");
    if table.has_string_pool {
        out.push_str("    pub fn resolve_string(&self, key: StringKey) -> Option<&str> {\n");
        out.push_str("        self.string_pool.get(&key).map(|value| value.as_str())\n");
        out.push_str("    }\n\n");
    }
    for field in &table.secondary_indexes {
        if let Some(index_key_type) = field.csv_type.index_key_type() {
            out.push_str(&format!(
                "    pub fn find_by_{}(&self, value: {}) -> Vec<&{}> {{\n",
                field.rust_name, index_key_type, table.row_struct_name
            ));
            out.push_str(&format!("        self.by_{}\n", field.rust_name));
            out.push_str("            .get(&value)\n");
            out.push_str("            .map(|row_indexes| {\n");
            out.push_str("                row_indexes\n");
            out.push_str("                    .iter()\n");
            out.push_str("                    .filter_map(|&row_index| self.rows.get(row_index))\n");
            out.push_str("                    .collect()\n");
            out.push_str("            })\n");
            out.push_str("            .unwrap_or_default()\n");
            out.push_str("    }\n\n");
        }
    }
    out.push_str("}\n\n");
}
fn builtin_indexed_columns() -> BTreeMap<&'static str, Vec<&'static str>> {
    BTreeMap::from([
        ("TestTable_100", vec!["Id", "Field_2", "Field_6"]),
        ("TestTable_110", vec!["Id", "Field_0"]),
    ])
}

fn write_if_changed(path: &Path, contents: &str) -> std::io::Result<()> {
    match fs::read_to_string(path) {
        Ok(existing) if existing == contents => return Ok(()),
        Ok(_) | Err(_) => {}
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(path, contents)
}

fn to_pascal_case(value: &str) -> String {
    let mut result = String::new();
    for part in split_identifier(value) {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            result.push(first.to_ascii_uppercase());
            result.push_str(chars.as_str());
        }
    }
    result
}

fn to_snake_case(value: &str) -> String {
    let result = split_identifier(value)
        .into_iter()
        .map(|part| part.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("_");
    // Escape Rust keywords
    if is_rust_keyword(&result) {
        format!("{}_", result)
    } else {
        result
    }
}

fn is_rust_keyword(s: &str) -> bool {
    matches!(
        s,
        "as" | "async" | "await" | "break" | "const" | "continue" | "crate" | "dyn"
            | "else" | "enum" | "extern" | "false" | "fn" | "for" | "if" | "impl" | "in"
            | "let" | "loop" | "match" | "mod" | "move" | "mut" | "pub" | "ref" | "return"
            | "self" | "Self" | "static" | "struct" | "super" | "trait" | "true" | "type"
            | "unsafe" | "use" | "where" | "while"
    )
}

fn to_upper_snake_case(value: &str) -> String {
    split_identifier(value)
        .into_iter()
        .map(|part| part.to_ascii_uppercase())
        .collect::<Vec<_>>()
        .join("_")
}

fn split_identifier(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();

    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            parts.push(std::mem::take(&mut current));
        }
    }

    if !current.is_empty() {
        parts.push(current);
    }

    if parts.is_empty() {
        vec!["Generated".to_string()]
    } else {
        parts
    }
}



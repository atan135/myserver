use std::collections::HashMap;
use std::path::Path;

pub trait CsvTableLoader: Sized {
    const TABLE_NAME: &'static str;
    const SCHEMA_SIGNATURE: &'static str;

    fn load_from_csv(path: &Path) -> Result<Self, CsvLoadError>;
}

#[derive(Debug)]
pub enum CsvLoadError {
    Io(std::io::Error),
    InvalidSchema(String),
    InvalidRow(String),
    Parse(String),
}

impl std::fmt::Display for CsvLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::InvalidSchema(message) => write!(f, "{message}"),
            Self::InvalidRow(message) => write!(f, "{message}"),
            Self::Parse(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for CsvLoadError {}

impl From<std::io::Error> for CsvLoadError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

pub struct CsvRowReader<'a> {
    table_name: &'static str,
    row_index: usize,
    fields: &'a [String],
}

impl<'a> CsvRowReader<'a> {
    pub fn new(table_name: &'static str, row_index: usize, fields: &'a [String]) -> Self {
        Self { table_name, row_index, fields }
    }

    pub fn field(&self, index: usize, name: &str) -> Result<&str, CsvLoadError> {
        self.fields.get(index).map(|value| value.as_str()).ok_or_else(|| {
            CsvLoadError::InvalidRow(format!(
                "table {} row {} missing field `{}` at column {}",
                self.table_name, self.row_index, name, index
            ))
        })
    }

    pub fn parse_i32(&self, index: usize, name: &str) -> Result<i32, CsvLoadError> {
        self.field(index, name)?.parse::<i32>().map_err(|error| self.parse_error(name, error))
    }

    pub fn parse_i64(&self, index: usize, name: &str) -> Result<i64, CsvLoadError> {
        self.field(index, name)?.parse::<i64>().map_err(|error| self.parse_error(name, error))
    }

    pub fn parse_f32(&self, index: usize, name: &str) -> Result<f32, CsvLoadError> {
        self.field(index, name)?.parse::<f32>().map_err(|error| self.parse_error(name, error))
    }

    pub fn parse_string_key(&self, index: usize, name: &str, string_pool: &mut StringPoolBuilder) -> Result<u32, CsvLoadError> {
        Ok(string_pool.intern(self.field(index, name)?))
    }

    pub fn parse_string_array(&self, index: usize, name: &str, string_pool: &mut StringPoolBuilder) -> Result<Vec<u32>, CsvLoadError> {
        let value = self.field(index, name)?;
        if value.is_empty() {
            return Ok(Vec::new());
        }

        Ok(value.split('|').map(|item| string_pool.intern(item.trim())).collect())
    }

    pub fn parse_string_int_dict(&self, index: usize, name: &str, string_pool: &mut StringPoolBuilder) -> Result<HashMap<u32, i32>, CsvLoadError> {
        let value = self.field(index, name)?;
        if value.is_empty() {
            return Ok(HashMap::new());
        }

        let mut map = HashMap::new();
        for part in value.split('|') {
            let (key, raw_value) = part.split_once(':').ok_or_else(|| {
                CsvLoadError::Parse(format!(
                    "table {} row {} field `{}` contains invalid dict entry `{}`",
                    self.table_name, self.row_index, name, part
                ))
            })?;
            let string_key = string_pool.intern(key.trim());
            let parsed_value = raw_value.trim().parse::<i32>().map_err(|error| self.parse_error(name, error))?;
            map.insert(string_key, parsed_value);
        }
        Ok(map)
    }

    fn parse_error(&self, name: &str, error: impl std::fmt::Display) -> CsvLoadError {
        CsvLoadError::Parse(format!(
            "table {} row {} field `{}` parse failed: {}",
            self.table_name, self.row_index, name, error
        ))
    }
}

#[derive(Default)]
pub struct StringPoolBuilder {
    next_key: u32,
    values: HashMap<u32, String>,
    reverse: HashMap<String, u32>,
}

impl StringPoolBuilder {
    pub fn intern(&mut self, value: &str) -> u32 {
        if let Some(&existing_key) = self.reverse.get(value) {
            return existing_key;
        }

        let key = self.next_key;
        self.next_key += 1;
        let owned = value.to_string();
        self.values.insert(key, owned.clone());
        self.reverse.insert(owned, key);
        key
    }

    pub fn finish(self) -> HashMap<u32, String> {
        self.values
    }
}

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct SchemaAst {
    pub models: HashMap<String, ModelNode>,
    pub unions: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelNode {
    pub name: String,
    pub fields: Vec<FieldNode>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AstFieldType {
    Scalar(String),
    ScalarArray(String),
    Relation(String),          // Points to another Model
    PolymorphicUnion(String),  // Points to a defined Union
    // Extension for custom arrays, though not explicitly in the snippet
    RelationArray(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum DefaultFunc {
    AutoIncrement,
    Now,
    Uuid,
    Cuid,
    Static(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldAttribute {
    Id,
    Unique,
    UpdatedAt,
    Ignore,
    Map(String),
    Default(DefaultFunc),
    Relation { name: Option<String>, fields: Vec<String>, references: Vec<String>, on_delete: Option<String>, deferrable: bool, column: Option<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldNode {
    pub name: String,
    pub field_type: AstFieldType,
    pub attributes: Vec<FieldAttribute>,
}

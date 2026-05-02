use std::collections::{HashMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SchemaAst {
    pub models: HashMap<String, ModelNode>,
    pub bases: HashMap<String, BaseNode>,
    pub unions: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct BaseNode {
    pub name: String,
    pub fields: Vec<FieldNode>,
    pub extends: Vec<String>,
    // --- COMPILER STATE (Hydrated in Phase 2) ---
    pub resolved_fields: Vec<FieldNode>,
    pub resolved_bases: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelNode {
    pub name: String,
    pub fields: Vec<FieldNode>,
    pub extends: Vec<String>,
    pub block_attributes: Vec<ModelAttribute>,
    // --- COMPILER STATE (Hydrated in Phase 2) ---
    pub resolved_fields: Vec<FieldNode>,
    pub resolved_bases: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModelAttribute {
    Id(DefaultFunc),
    Track,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AstFieldType {
    Scalar(String),
    ScalarArray(String),
    Relation(String),          // Points to another Model
    PolymorphicUnion(String),  // Points to a defined Union
    PolymorphicUnionArray(String),
    PolymorphicBase(String),       // Points to a defined Base
    PolymorphicBaseArray(String),  // Array of defined Base
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
    InternalTracked,
    Map(String),
    InternalDefault(DefaultFunc),
    Relation { name: Option<String>, fields: Vec<String>, references: Vec<String>, on_delete: Option<String>, deferrable: bool, column: Option<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldNode {
    pub name: String,
    pub field_type: AstFieldType,
    pub is_optional: bool,
    pub attributes: Vec<FieldAttribute>,
}

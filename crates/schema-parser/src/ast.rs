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
pub struct FieldNode {
    pub name: String,
    pub field_type: AstFieldType,
    pub attributes: Vec<String>,
}

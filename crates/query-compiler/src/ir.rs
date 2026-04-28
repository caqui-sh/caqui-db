use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct QueryNode {
    pub target_model: String,
    pub alias: String, // Crucial for preventing namespace collisions in self-joins (e.g., t0, t1)
    pub selections: Vec<SelectField>,
    pub filters: Option<WhereClause>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SelectField {
    /// A standard primitive column (e.g., 'id', 'name')
    Scalar(String),
    /// A standard SQLite TEXT column containing a JSON array (e.g., 'tags')
    ScalarArray(String),
    /// A nested 1:N or N:M relationship, pointing to a sub-QueryNode
    Relation {
        field_name: String,
        foreign_key: String,
        is_list: bool,
        query: Box<QueryNode>,
    },
    /// A Polymorphic Union request requiring conditional resolution
    PolymorphicUnion {
        field_name: String,
        target_fragments: HashMap<String, QueryNode>, // e.g., "Article" -> QueryNode
    }
}

// WhereClause AST definitions stubbed out for Phase 4
#[derive(Debug, Clone, PartialEq)]
pub enum WhereClause {
    Eq(String, String),
    // ...
}

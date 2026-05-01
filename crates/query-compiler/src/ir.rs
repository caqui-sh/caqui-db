use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct QueryNode {
    pub target_model: String,
    pub alias: String, // Crucial for preventing namespace collisions in self-joins (e.g., t0, t1)
    pub selections: Vec<SelectField>,
    pub filters: Option<WhereClause>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SelectField {
    /// A standard primitive column (e.g., 'id', 'name')
    Scalar(String),
    /// A boolean primitive column, requires special JSON casting in SQLite
    ScalarBoolean(String),
    /// A standard SQLite TEXT column containing a JSON array (e.g., 'tags')
    ScalarArray(String),
    /// A nested 1:N or N:M relationship, pointing to a sub-QueryNode
    Relation {
        field_name: String,
        foreign_key: String,
        is_list: bool,
        is_forward: bool,
        query: Box<QueryNode>,
    },
    /// A Polymorphic Union request requiring conditional resolution
    PolymorphicUnion {
        field_name: String,
        is_list: bool,
        target_fragments: HashMap<String, QueryNode>, // e.g., "Article" -> QueryNode
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum WhereCondition {
    Eq(String),
    NotEq(String),
    Gt(String),
    Gte(String),
    Lt(String),
    Lte(String),
    In(Vec<String>),
    IsNull,
    IsNotNull,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RelationFilter {
    Some(Box<WhereClause>),
    Every(Box<WhereClause>),
    None(Box<WhereClause>),
    Is(Box<WhereClause>),
    IsNot(Box<WhereClause>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum WhereClause {
    And(Vec<WhereClause>),
    Or(Vec<WhereClause>),
    Field(String, WhereCondition),
    Relation {
        field_name: String,
        target_model: String,
        fk_column: String,
        is_forward: bool,
        filter: RelationFilter,
    },
    AlwaysTrue,
}

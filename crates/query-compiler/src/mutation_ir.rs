use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum Parameter {
    Literal(Value),
    /// Instructs the engine to replace this with the returned value from a previous step
    Reference { step_id: String, column: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionStep {
    Query {
        id: String,
        sql: String,
        params: Vec<Parameter>,
    },
    UpsertBranch {
        check_sql: String,
        check_params: Vec<Parameter>,
        if_exists: Vec<ExecutionStep>,
        if_not_exists: Vec<ExecutionStep>,
        root_step_id: String,
    },
    UpdateBranch {
        id: String,
        sql: String,
        params: Vec<Parameter>,
        parent_ref: Parameter,
    },
    DeleteBranch {
        id: String,
        sql: String,
        params: Vec<Parameter>,
        parent_ref: Parameter,
    },
    UpdateMany {
        id: String,
        queries: Vec<(String, Vec<Parameter>)>,
        parent_ref: Option<Parameter>,
    },
    DeleteMany {
        id: String,
        queries: Vec<(String, Vec<Parameter>)>,
        parent_ref: Option<Parameter>,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionPlan {
    pub root_step_id: String,
    pub steps: Vec<ExecutionStep>,
}

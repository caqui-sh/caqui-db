use deadpool_sqlite::Pool;
use serde_json::Value;
use std::collections::HashMap;
use query_compiler::mutation_ir::{ExecutionPlan, ExecutionStep, Parameter};

pub async fn execute_compiled_read(pool: &Pool, sql: String) -> Result<Value, String> {
    let conn = pool.get().await.map_err(|e| e.to_string())?;
    
    let json_payload = conn.interact(move |db| -> Result<String, rusqlite::Error> {
        let mut stmt = db.prepare_cached(&sql)?;
        let raw_json: String = stmt.query_row([], |row| row.get(0))?;
        Ok(raw_json)
    }).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;

    let parsed_data: Value = serde_json::from_str(&json_payload).map_err(|e| e.to_string())?;

    Ok(parsed_data)
}

fn execute_steps(
    tx: &rusqlite::Transaction,
    steps: &[ExecutionStep],
    returned_values: &mut HashMap<String, String>,
    root_step_id: &str,
    root_id: &mut String,
) -> Result<(), rusqlite::Error> {
    for step in steps {
        match step {
            ExecutionStep::Query { id, sql, params } => {
                let mut stmt = tx.prepare_cached(sql)?;
                let mut sql_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
                for param in params {
                    match param {
                        Parameter::Literal(val) => {
                            if let Some(s) = val.as_str() {
                                sql_params.push(Box::new(s.to_string()));
                            } else if let Some(n) = val.as_i64() {
                                sql_params.push(Box::new(n));
                            } else if let Some(n) = val.as_f64() {
                                sql_params.push(Box::new(n));
                            } else if let Some(b) = val.as_bool() {
                                sql_params.push(Box::new(b));
                            } else if val.is_null() {
                                sql_params.push(Box::new(rusqlite::types::Null));
                            } else {
                                return Err(rusqlite::Error::ToSqlConversionFailure(
                                    Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, "Unsupported JSON value for scalar binding"))
                                ));
                            }
                        },
                        Parameter::Reference { step_id, .. } => {
                            if let Some(val) = returned_values.get(step_id) {
                                sql_params.push(Box::new(val.clone()));
                            } else {
                                return Err(rusqlite::Error::ToSqlConversionFailure(
                                    Box::new(std::io::Error::new(std::io::ErrorKind::NotFound, "Missing reference ID"))
                                ));
                            }
                        }
                    }
                }

                let borrowed_params: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(|b| &**b).collect();
                let returned_id: String = match stmt.query_row(&borrowed_params[..], |row| row.get(0)) {
                    Ok(id) => id,
                    Err(rusqlite::Error::QueryReturnedNoRows) => {
                        if id.contains("_disconnect") || id.contains("_set") || id.contains("_cascade") {
                            "".to_string()
                        } else {
                            return Err(rusqlite::Error::ToSqlConversionFailure(
                                Box::new(std::io::Error::new(std::io::ErrorKind::NotFound, "Record not found"))
                            ));
                        }
                    }
                    Err(e) => return Err(e),
                };
                
                if id == root_step_id {
                    *root_id = returned_id.clone();
                }
                
                returned_values.insert(id.clone(), returned_id);
            },
            ExecutionStep::UpsertBranch { check_sql, check_params, if_exists, if_not_exists, root_step_id: branch_root_id } => {
                let mut stmt = tx.prepare_cached(check_sql)?;
                let mut sql_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
                for param in check_params {
                    match param {
                        Parameter::Literal(val) => {
                            if let Some(s) = val.as_str() {
                                sql_params.push(Box::new(s.to_string()));
                            } else if let Some(n) = val.as_i64() {
                                sql_params.push(Box::new(n));
                            } else if let Some(n) = val.as_f64() {
                                sql_params.push(Box::new(n));
                            } else if let Some(b) = val.as_bool() {
                                sql_params.push(Box::new(b));
                            } else if val.is_null() {
                                sql_params.push(Box::new(rusqlite::types::Null));
                            } else {
                                return Err(rusqlite::Error::ToSqlConversionFailure(
                                    Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, "Unsupported JSON value for scalar binding"))
                                ));
                            }
                        },
                        Parameter::Reference { step_id, .. } => {
                            if let Some(val) = returned_values.get(step_id) {
                                sql_params.push(Box::new(val.clone()));
                            } else {
                                return Err(rusqlite::Error::ToSqlConversionFailure(
                                    Box::new(std::io::Error::new(std::io::ErrorKind::NotFound, "Missing reference ID"))
                                ));
                            }
                        }
                    }
                }

                let borrowed_params: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(|b| &**b).collect();
                let exists_id: Option<String> = match stmt.query_row(&borrowed_params[..], |row| row.get(0)) {
                    Ok(id) => Some(id),
                    Err(rusqlite::Error::QueryReturnedNoRows) => None,
                    Err(e) => return Err(e),
                };

                let execution_target = if exists_id.is_some() {
                    if_exists
                } else {
                    if_not_exists
                };
                
                let mut temp_root_id = String::new();
                // The root step of the inner plan is the FIRST step of the plan.
                let inner_root_step_id = if let Some(first_step) = execution_target.first() {
                    match first_step {
                        ExecutionStep::Query { id, .. } => id.clone(),
                        ExecutionStep::UpsertBranch { root_step_id, .. } => root_step_id.clone(),
                    }
                } else {
                    String::new()
                };
                
                execute_steps(tx, execution_target, returned_values, &inner_root_step_id, &mut temp_root_id)?;
                
                if branch_root_id == root_step_id {
                    *root_id = temp_root_id.clone();
                }
                
                returned_values.insert(branch_root_id.clone(), temp_root_id);
            }
        }
    }
    Ok(())
}

pub async fn execute_mutation_plan(pool: &Pool, plan: ExecutionPlan) -> Result<String, String> {
    let conn = pool.get().await.map_err(|e| e.to_string())?;

    let result = conn.interact(move |db| -> Result<String, rusqlite::Error> {
        let tx = db.transaction()?;
        let mut returned_values: HashMap<String, String> = HashMap::new();
        let mut root_id = String::new();

        execute_steps(&tx, &plan.steps, &mut returned_values, &plan.root_step_id, &mut root_id)?;

        tx.commit()?;
        Ok(root_id)
    }).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;

    Ok(result)
}

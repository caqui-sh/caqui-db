pub enum ArrayOp {
    Push(String),
    RemoveIndex(usize),
}

pub fn compile_array_mutation(table: &str, field: &str, op: ArrayOp, id: &str) -> String {
    match op {
        // '$[#]' is SQLite json1 syntax for "append to the end of the array"
        // COALESCE ensures initialization if the column is currently NULL.
        ArrayOp::Push(val) => format!(
            "UPDATE {} SET {} = json_insert(COALESCE({}, '[]'), '$[#]', '{}') WHERE id = '{}';",
            table, field, field, val, id
        ),
        // '$[X]' targets a specific index for removal.
        ArrayOp::RemoveIndex(idx) => format!(
            "UPDATE {} SET {} = json_remove({}, '$[{}]') WHERE id = '{}';",
            table, field, field, idx, id
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compile_array_push() {
        let sql = compile_array_mutation("User", "tags", ArrayOp::Push("developer".to_string()), "user_1");
        assert_eq!(sql, "UPDATE User SET tags = json_insert(COALESCE(tags, '[]'), '$[#]', 'developer') WHERE id = 'user_1';");
    }

    #[test]
    fn test_compile_array_remove_index() {
        let sql = compile_array_mutation("User", "tags", ArrayOp::RemoveIndex(2), "user_1");
        assert_eq!(sql, "UPDATE User SET tags = json_remove(tags, '$[2]') WHERE id = 'user_1';");
    }
}

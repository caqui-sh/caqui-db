use crate::ir::{QueryNode, SelectField, WhereClause, WhereCondition, RelationFilter};

pub fn compile_where_clause(clause: &WhereClause, alias: &str) -> String {
    match clause {
        WhereClause::And(clauses) => {
            let compiled: Vec<_> = clauses.iter().map(|c| compile_where_clause(c, alias)).collect();
            format!("({})", compiled.join(" AND "))
        }
        WhereClause::Or(clauses) => {
            let compiled: Vec<_> = clauses.iter().map(|c| compile_where_clause(c, alias)).collect();
            format!("({})", compiled.join(" OR "))
        }
        WhereClause::Field(field, condition) => {
            let col = format!("{}.{}", alias, field);
            match condition {
                WhereCondition::Eq(v) => format!("{} = '{}'", col, v.replace('\'', "''")),
                WhereCondition::NotEq(v) => format!("{} != '{}'", col, v.replace('\'', "''")),
                WhereCondition::Gt(v) => format!("{} > '{}'", col, v.replace('\'', "''")),
                WhereCondition::Gte(v) => format!("{} >= '{}'", col, v.replace('\'', "''")),
                WhereCondition::Lt(v) => format!("{} < '{}'", col, v.replace('\'', "''")),
                WhereCondition::Lte(v) => format!("{} <= '{}'", col, v.replace('\'', "''")),
                WhereCondition::In(vals) => {
                    if vals.is_empty() {
                        "1=0".to_string()
                    } else {
                        let escaped: Vec<_> = vals.iter().map(|v| format!("'{}'", v.replace('\'', "''"))).collect();
                        format!("{} IN ({})", col, escaped.join(", "))
                    }
                },
                WhereCondition::IsNull => format!("{} IS NULL", col),
                WhereCondition::IsNotNull => format!("{} IS NOT NULL", col),
            }
        }
        WhereClause::Relation { target_model, fk_column, is_forward, filter, .. } => {
            let child_alias = format!("{}_{}", alias, target_model.to_lowercase());
            
            let join_cond = if *is_forward {
                // Parent holds FK
                format!("{}.id = {}.{}", child_alias, alias, fk_column)
            } else {
                // Child holds FK
                format!("{}.{} = {}.id", child_alias, fk_column, alias)
            };

            match filter {
                RelationFilter::Some(inner) => {
                    let inner_sql = compile_where_clause(inner, &child_alias);
                    format!("EXISTS (SELECT 1 FROM {} AS {} WHERE {} AND {})", target_model, child_alias, join_cond, inner_sql)
                }
                RelationFilter::Every(inner) => {
                    let inner_sql = compile_where_clause(inner, &child_alias);
                    format!("NOT EXISTS (SELECT 1 FROM {} AS {} WHERE {} AND NOT ({}))", target_model, child_alias, join_cond, inner_sql)
                }
                RelationFilter::None(inner) => {
                    let inner_sql = compile_where_clause(inner, &child_alias);
                    format!("NOT EXISTS (SELECT 1 FROM {} AS {} WHERE {} AND {})", target_model, child_alias, join_cond, inner_sql)
                }
                RelationFilter::Is(inner) => {
                    let inner_sql = compile_where_clause(inner, &child_alias);
                    format!("EXISTS (SELECT 1 FROM {} AS {} WHERE {} AND {})", target_model, child_alias, join_cond, inner_sql)
                }
                RelationFilter::IsNot(inner) => {
                    let inner_sql = compile_where_clause(inner, &child_alias);
                    format!("NOT EXISTS (SELECT 1 FROM {} AS {} WHERE {} AND {})", target_model, child_alias, join_cond, inner_sql)
                }
            }
        }
        WhereClause::AlwaysTrue => "1=1".to_string(),
    }
}

pub fn compile_select(node: &QueryNode, parent_ref: Option<(&str, &str)>) -> String {
    let mut json_pairs = Vec::new();

    for selection in &node.selections {
        match selection {
            SelectField::Scalar(name) => {
                // Generates: 'name', t0.name
                json_pairs.push(format!("'{}', {}.{}", name, node.alias, name));
            },
            SelectField::ScalarArray(name) => {
                // json() forces SQLite to parse the TEXT column as valid JSON before embedding, 
                // preventing double-escaped strings like "[\"a\"]".
                json_pairs.push(format!("'{}', json({}.{})", name, node.alias, name));
            },
            SelectField::Relation { field_name, foreign_key, is_list, is_forward, query } => {
                // The Recursive N+1 Neutralizer: Correlated Subquery with JSON aggregation
                let child_json_obj = compile_select(query, Some((&node.alias, foreign_key)));
                
                let mut where_conds = if *is_forward {
                    vec![format!("{}.id = {}.{}", query.alias, node.alias, foreign_key)]
                } else {
                    vec![format!("{}.{} = {}.id", query.alias, foreign_key, node.alias)]
                };
                if let Some(filters) = &query.filters {
                    where_conds.push(compile_where_clause(filters, &query.alias));
                }
                let where_str = where_conds.join(" AND ");

                let mut limit_offset = String::new();
                if *is_list {
                    if let Some(l) = query.limit {
                        limit_offset.push_str(&format!(" LIMIT {}", l));
                    }
                } else {
                    limit_offset.push_str(" LIMIT 1");
                }
                if let Some(o) = query.offset {
                    limit_offset.push_str(&format!(" OFFSET {}", o));
                }
                
                let subquery = if *is_list {
                    format!("(SELECT json_group_array({}) FROM {} AS {} WHERE {}{})",
                        child_json_obj, query.target_model, query.alias, where_str, limit_offset)
                } else {
                    format!("(SELECT {} FROM {} AS {} WHERE {}{})",
                        child_json_obj, query.target_model, query.alias, where_str, limit_offset)
                };
                
                json_pairs.push(format!("'{}', {}", field_name, subquery));
            },
            SelectField::PolymorphicUnion { field_name, is_list, target_fragments } => {
                if *is_list {
                    unimplemented!("Polymorphic union arrays are currently supported in the AST and Schema, but not yet implemented in the Query Compiler.");
                }
                let type_col = format!("{}.{}_type", node.alias, field_name); // e.g., t0.result_type
                let id_col = format!("{}.{}_id", node.alias, field_name);     // e.g., t0.result_id
                
                let mut case_statements = Vec::new();
                
                // We sort target_fragments keys to ensure deterministic query generation,
                // which is helpful for unit testing.
                let mut fragment_keys: Vec<_> = target_fragments.keys().collect();
                fragment_keys.sort();
                
                for model_name in fragment_keys {
                    let fragment_node = target_fragments.get(model_name).unwrap();
                    let sub_obj = compile_select(fragment_node, Some((&node.alias, &id_col)));
                    
                    let mut where_conds = vec![format!("{}.id = {}", fragment_node.alias, id_col)];
                    if let Some(filters) = &fragment_node.filters {
                        where_conds.push(compile_where_clause(filters, &fragment_node.alias));
                    }
                    let where_str = where_conds.join(" AND ");

                    case_statements.push(format!(
                        "WHEN '{}' THEN (SELECT {} FROM {} AS {} WHERE {})",
                        model_name,          // e.g., 'Article'
                        sub_obj,             // e.g., json_object('title', t1.title, '__typename', 'Article')
                        model_name,          // Target table
                        fragment_node.alias, // Child table alias
                        where_str
                    ));
                }
                
                // Generates: 'result', CASE t0.result_type WHEN 'Article' THEN (...) ELSE NULL END
                json_pairs.push(format!("'{}', CASE {} {} ELSE NULL END", field_name, type_col, case_statements.join(" ")));
            }
        }
    }

    let json_obj = format!("json_object({})", json_pairs.join(", "));
    
    if parent_ref.is_none() {
        let mut root_where = String::new();
        if let Some(filters) = &node.filters {
            root_where = format!(" WHERE {}", compile_where_clause(filters, &node.alias));
        }
        
        let mut limit_clause = String::new();
        if let Some(l) = node.limit {
            limit_clause = format!(" LIMIT {}", l);
        }
        
        let mut offset_clause = String::new();
        if let Some(o) = node.offset {
            offset_clause = format!(" OFFSET {}", o);
        }

        // Root query: Wrap execution in a final SELECT returning a JSON array
        format!("SELECT json_group_array({}) AS payload FROM {} AS {}{}{}{};", json_obj, node.target_model, node.alias, root_where, limit_clause, offset_clause)
    } else {
        // Child query: Return inner object formulation for subquery injection
        json_obj
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_compile_basic_select() {
        let query = QueryNode {
            target_model: "User".to_string(),
            alias: "t0".to_string(),
            selections: vec![
                SelectField::Scalar("id".to_string()),
                SelectField::Scalar("name".to_string()),
            ],
            filters: None,
            limit: None,
            offset: None,
        };
        let sql = compile_select(&query, None);
        assert_eq!(sql, "SELECT json_group_array(json_object('id', t0.id, 'name', t0.name)) AS payload FROM User AS t0;");
    }

    #[test]
    fn test_compile_relation_select() {
        let child_query = QueryNode {
            target_model: "Post".to_string(),
            alias: "t1".to_string(),
            selections: vec![
                SelectField::Scalar("id".to_string()),
                SelectField::Scalar("title".to_string()),
            ],
            filters: None,
            limit: None,
            offset: None,
        };
        
        let query = QueryNode {
            target_model: "User".to_string(),
            alias: "t0".to_string(),
            selections: vec![
                SelectField::Scalar("id".to_string()),
                SelectField::Relation {
                    field_name: "posts".to_string(),
                    foreign_key: "author_id".to_string(),
                    is_list: true,
                    is_forward: false,
                    query: Box::new(child_query),
                }
            ],
            filters: None,
            limit: None,
            offset: None,
        };
        let sql = compile_select(&query, None);
        assert_eq!(
            sql, 
            "SELECT json_group_array(json_object('id', t0.id, 'posts', (SELECT json_group_array(json_object('id', t1.id, 'title', t1.title)) FROM Post AS t1 WHERE t1.author_id = t0.id))) AS payload FROM User AS t0;"
        );
    }
    
    #[test]
    fn test_compile_polymorphic_union() {
        let article_fragment = QueryNode {
            target_model: "Article".to_string(),
            alias: "t1".to_string(),
            selections: vec![
                SelectField::Scalar("id".to_string()),
                SelectField::Scalar("title".to_string()),
            ],
            filters: None,
            limit: None,
            offset: None,
        };
        
        let mut fragments = HashMap::new();
        fragments.insert("Article".to_string(), article_fragment);
        
        let query = QueryNode {
            target_model: "User".to_string(),
            alias: "t0".to_string(),
            selections: vec![
                SelectField::Scalar("id".to_string()),
                SelectField::PolymorphicUnion {
                    field_name: "search".to_string(),
                    is_list: false,
                    target_fragments: fragments,
                }
            ],
            filters: None,
            limit: None,
            offset: None,
        };
        let sql = compile_select(&query, None);
        assert_eq!(
            sql,
            "SELECT json_group_array(json_object('id', t0.id, 'search', CASE t0.search_type WHEN 'Article' THEN (SELECT json_object('id', t1.id, 'title', t1.title) FROM Article AS t1 WHERE t1.id = t0.search_id) ELSE NULL END)) AS payload FROM User AS t0;"
        );
    }

    #[test]
    fn test_compile_deep_recursive_relation() {
        let comments_query = QueryNode {
            target_model: "Comment".to_string(),
            alias: "t2".to_string(),
            selections: vec![
                SelectField::Scalar("id".to_string()),
                SelectField::Scalar("body".to_string()),
            ],
            filters: None,
            limit: None,
            offset: None,
        };
        
        let posts_query = QueryNode {
            target_model: "Post".to_string(),
            alias: "t1".to_string(),
            selections: vec![
                SelectField::Scalar("id".to_string()),
                SelectField::Relation {
                    field_name: "comments".to_string(),
                    foreign_key: "post_id".to_string(),
                    is_list: true,
                    is_forward: false,
                    query: Box::new(comments_query),
                }
            ],
            filters: None,
            limit: None,
            offset: None,
        };
        
        let user_query = QueryNode {
            target_model: "User".to_string(),
            alias: "t0".to_string(),
            selections: vec![
                SelectField::Scalar("id".to_string()),
                SelectField::Relation {
                    field_name: "posts".to_string(),
                    foreign_key: "author_id".to_string(),
                    is_list: true,
                    is_forward: false,
                    query: Box::new(posts_query),
                }
            ],
            filters: None,
            limit: None,
            offset: None,
        };
        
        let sql = compile_select(&user_query, None);
        assert_eq!(
            sql,
            "SELECT json_group_array(json_object('id', t0.id, 'posts', (SELECT json_group_array(json_object('id', t1.id, 'comments', (SELECT json_group_array(json_object('id', t2.id, 'body', t2.body)) FROM Comment AS t2 WHERE t2.post_id = t1.id))) FROM Post AS t1 WHERE t1.author_id = t0.id))) AS payload FROM User AS t0;"
        );
    }

    #[test]
    fn test_compile_scalar_array() {
        let query = QueryNode {
            target_model: "User".to_string(),
            alias: "t0".to_string(),
            selections: vec![
                SelectField::Scalar("id".to_string()),
                SelectField::ScalarArray("tags".to_string()),
            ],
            filters: None,
            limit: None,
            offset: None,
        };
        let sql = compile_select(&query, None);
        assert_eq!(sql, "SELECT json_group_array(json_object('id', t0.id, 'tags', json(t0.tags))) AS payload FROM User AS t0;");
    }

    #[test]
    fn test_compile_single_relation() {
        let child_query = QueryNode {
            target_model: "Profile".to_string(),
            alias: "t1".to_string(),
            selections: vec![
                SelectField::Scalar("bio".to_string()),
            ],
            filters: None,
            limit: None,
            offset: None,
        };
        
        let query = QueryNode {
            target_model: "User".to_string(),
            alias: "t0".to_string(),
            selections: vec![
                SelectField::Scalar("id".to_string()),
                SelectField::Relation {
                    field_name: "profile".to_string(),
                    foreign_key: "user_id".to_string(),
                    is_list: false,
                    is_forward: false,
                    query: Box::new(child_query),
                }
            ],
            filters: None,
            limit: None,
            offset: None,
        };
        let sql = compile_select(&query, None);
        assert_eq!(
            sql, 
            "SELECT json_group_array(json_object('id', t0.id, 'profile', (SELECT json_object('bio', t1.bio) FROM Profile AS t1 WHERE t1.user_id = t0.id LIMIT 1))) AS payload FROM User AS t0;"
        );
    }

    #[test]
    fn test_compile_multi_fragment_polymorphic_union() {
        let article_fragment = QueryNode {
            target_model: "Article".to_string(),
            alias: "t1".to_string(),
            selections: vec![SelectField::Scalar("title".to_string())],
            filters: None, limit: None, offset: None,
        };
        let video_fragment = QueryNode {
            target_model: "Video".to_string(),
            alias: "t2".to_string(),
            selections: vec![SelectField::Scalar("duration".to_string())],
            filters: None, limit: None, offset: None,
        };
        
        let mut fragments = HashMap::new();
        // Insert in reverse alphabetical to verify sorting
        fragments.insert("Video".to_string(), video_fragment);
        fragments.insert("Article".to_string(), article_fragment);
        
        let query = QueryNode {
            target_model: "User".to_string(),
            alias: "t0".to_string(),
            selections: vec![
                SelectField::Scalar("id".to_string()),
                SelectField::PolymorphicUnion {
                    field_name: "content".to_string(),
                    is_list: false,
                    target_fragments: fragments,
                }
            ],
            filters: None,
            limit: None,
            offset: None,
        };
        let sql = compile_select(&query, None);
        
        assert_eq!(
            sql,
            "SELECT json_group_array(json_object('id', t0.id, 'content', CASE t0.content_type WHEN 'Article' THEN (SELECT json_object('title', t1.title) FROM Article AS t1 WHERE t1.id = t0.content_id) WHEN 'Video' THEN (SELECT json_object('duration', t2.duration) FROM Video AS t2 WHERE t2.id = t0.content_id) ELSE NULL END)) AS payload FROM User AS t0;"
        );
    }

    #[test]
    fn test_compile_pagination_and_filtering() {
        let query = QueryNode {
            target_model: "User".to_string(),
            alias: "t0".to_string(),
            selections: vec![SelectField::Scalar("id".to_string())],
            filters: Some(WhereClause::Field("name".to_string(), WhereCondition::Eq("Alice".to_string()))),
            limit: Some(10),
            offset: Some(5),
        };
        let sql = compile_select(&query, None);
        assert_eq!(
            sql,
            "SELECT json_group_array(json_object('id', t0.id)) AS payload FROM User AS t0 WHERE t0.name = 'Alice' LIMIT 10 OFFSET 5;"
        );
    }

    #[test]
    fn test_compile_where_clause_complex() {
        let clause = WhereClause::And(vec![
            WhereClause::Field("age".to_string(), WhereCondition::Gte("18".to_string())),
            WhereClause::Or(vec![
                WhereClause::Field("status".to_string(), WhereCondition::Eq("active".to_string())),
                WhereClause::Field("status".to_string(), WhereCondition::Eq("pending".to_string())),
            ]),
            WhereClause::Field("name".to_string(), WhereCondition::In(vec!["Alice".to_string(), "Bob's".to_string()])),
        ]);
        let sql = compile_where_clause(&clause, "t0");
        assert_eq!(
            sql,
            "(t0.age >= '18' AND (t0.status = 'active' OR t0.status = 'pending') AND t0.name IN ('Alice', 'Bob''s'))"
        );
    }

    #[test]
    fn test_compile_where_clause_edge_cases() {
        let clause1 = WhereClause::Field("id".to_string(), WhereCondition::In(vec![]));
        assert_eq!(compile_where_clause(&clause1, "t0"), "1=0");

        let clause2 = WhereClause::Field("managerId".to_string(), WhereCondition::IsNull);
        assert_eq!(compile_where_clause(&clause2, "t1"), "t1.managerId IS NULL");

        let clause3 = WhereClause::Field("email".to_string(), WhereCondition::IsNotNull);
        assert_eq!(compile_where_clause(&clause3, "t2"), "t2.email IS NOT NULL");
    }

    #[test]
    fn test_compile_relation_with_pagination_and_filtering() {
        let child_query = QueryNode {
            target_model: "Post".to_string(),
            alias: "t1".to_string(),
            selections: vec![SelectField::Scalar("title".to_string())],
            filters: Some(WhereClause::Field("published".to_string(), WhereCondition::Eq("true".to_string()))),
            limit: Some(5),
            offset: Some(2),
        };
        
        let query = QueryNode {
            target_model: "User".to_string(),
            alias: "t0".to_string(),
            selections: vec![
                SelectField::Scalar("id".to_string()),
                SelectField::Relation {
                    field_name: "posts".to_string(),
                    foreign_key: "author_id".to_string(),
                    is_list: true,
                    is_forward: false,
                    query: Box::new(child_query),
                }
            ],
            filters: None,
            limit: None,
            offset: None,
        };
        
        let sql = compile_select(&query, None);
        assert_eq!(
            sql,
            "SELECT json_group_array(json_object('id', t0.id, 'posts', (SELECT json_group_array(json_object('title', t1.title)) FROM Post AS t1 WHERE t1.author_id = t0.id AND t1.published = 'true' LIMIT 5 OFFSET 2))) AS payload FROM User AS t0;"
        );
    }

    #[test]
    fn test_compile_polymorphic_union_with_filtering() {
        let article_fragment = QueryNode {
            target_model: "Article".to_string(),
            alias: "t1".to_string(),
            selections: vec![SelectField::Scalar("title".to_string())],
            filters: Some(WhereClause::Field("status".to_string(), WhereCondition::Eq("published".to_string()))),
            limit: None, offset: None,
        };
        
        let mut fragments = HashMap::new();
        fragments.insert("Article".to_string(), article_fragment);
        
        let query = QueryNode {
            target_model: "User".to_string(),
            alias: "t0".to_string(),
            selections: vec![
                SelectField::Scalar("id".to_string()),
                SelectField::PolymorphicUnion {
                    field_name: "search".to_string(),
                    is_list: false,
                    target_fragments: fragments,
                }
            ],
            filters: None,
            limit: None,
            offset: None,
        };
        
        let sql = compile_select(&query, None);
        assert_eq!(
            sql,
            "SELECT json_group_array(json_object('id', t0.id, 'search', CASE t0.search_type WHEN 'Article' THEN (SELECT json_object('title', t1.title) FROM Article AS t1 WHERE t1.id = t0.search_id AND t1.status = 'published') ELSE NULL END)) AS payload FROM User AS t0;"
        );
    }
}

use crate::ir::{QueryNode, SelectField, WhereClause, WhereCondition, RelationFilter, QueryIrSource};
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
                format!("{}.__id = {}.{}", child_alias, alias, fk_column)
            } else {
                // Child holds FK
                format!("{}.{} = {}.__id", child_alias, fk_column, alias)
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
            SelectField::ScalarBoolean(name) => {
                // Generates: 'isPublished', CASE t0.isPublished WHEN 1 THEN json('true') WHEN 0 THEN json('false') ELSE NULL END
                json_pairs.push(format!("'{}', CASE {}.{} WHEN 1 THEN json('true') WHEN 0 THEN json('false') ELSE NULL END", name, node.alias, name));
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
                    vec![format!("{}.{} = {}.{}", query.alias, query.primary_key, node.alias, foreign_key)]
                } else {
                    vec![format!("{}.{} = {}.{}", query.alias, foreign_key, node.alias, node.primary_key)]
                };
                if let Some(filters) = &query.filters {
                    where_conds.push(compile_where_clause(filters, &query.alias));
                }
                let where_str = where_conds.join(" AND ");

                let mut order_by_sql = String::new();
                if !query.order_by.is_empty() {
                    let orders: Vec<_> = query.order_by.iter().map(|(field, dir)| {
                        let dir_str = match dir {
                            crate::ir::OrderDirection::Asc => "ASC",
                            crate::ir::OrderDirection::Desc => "DESC",
                        };
                        format!("{}.{} {}", query.alias, field, dir_str)
                    }).collect();
                    order_by_sql = format!(" ORDER BY {}", orders.join(", "));
                }

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
                    let source_table = match &query.source {
                        QueryIrSource::Table(t) => t.clone(),
                        QueryIrSource::Polymorphic { alias: _, branches } => {
                            let mut inner_branch_sqls = Vec::new();
                            for branch in branches {
                                let mut branch_selects = Vec::new();
                                for sel in &branch.selections {
                                    match sel {
                                        SelectField::Scalar(name) | SelectField::ScalarArray(name) | SelectField::ScalarBoolean(name) => {
                                            branch_selects.push(format!("{}.{}", branch.alias, name))
                                        },
                                        SelectField::SyntheticNull(name) => branch_selects.push(name.clone()),
                                        _ => {}
                                    }
                                }
                                let b_where = branch.filters.as_ref().map(|w| format!(" WHERE {}", compile_where_clause(w, &branch.alias))).unwrap_or_default();
                                let b_table = match &branch.source { QueryIrSource::Table(t) => t.clone(), _ => panic!() };
                                inner_branch_sqls.push(format!("SELECT {} FROM {} AS {}{}", branch_selects.join(", "), b_table, branch.alias, b_where));
                            }
                            format!("(\n{}\n)", inner_branch_sqls.join("\nUNION ALL\n"))
                        },
                    };
                    format!("(SELECT json_group_array({}) FROM {} AS {} WHERE {}{}{})",
                        child_json_obj, source_table, query.alias, where_str, order_by_sql, limit_offset)
                } else {
                    let source_table = match &query.source {
                        QueryIrSource::Table(t) => t.clone(),
                        QueryIrSource::Polymorphic { alias: _, branches } => {
                            let mut inner_branch_sqls = Vec::new();
                            for branch in branches {
                                let mut branch_selects = Vec::new();
                                for sel in &branch.selections {
                                    match sel {
                                        SelectField::Scalar(name) | SelectField::ScalarArray(name) | SelectField::ScalarBoolean(name) => {
                                            branch_selects.push(format!("{}.{}", branch.alias, name))
                                        },
                                        SelectField::SyntheticNull(name) => branch_selects.push(name.clone()),
                                        _ => {}
                                    }
                                }
                                let b_where = branch.filters.as_ref().map(|w| format!(" WHERE {}", compile_where_clause(w, &branch.alias))).unwrap_or_default();
                                let b_table = match &branch.source { QueryIrSource::Table(t) => t.clone(), _ => panic!() };
                                inner_branch_sqls.push(format!("SELECT {} FROM {} AS {}{}", branch_selects.join(", "), b_table, branch.alias, b_where));
                            }
                            format!("(\n{}\n)", inner_branch_sqls.join("\nUNION ALL\n"))
                        },
                    };
                    format!("(SELECT {} FROM {} AS {} WHERE {}{})",
                        child_json_obj, source_table, query.alias, where_str, limit_offset)
                };
                
                json_pairs.push(format!("'{}', {}", field_name, subquery));
            },
            SelectField::Polymorphic { field_name, is_list, target_fragments } => {
                let mut fragment_keys: Vec<_> = target_fragments.keys().collect();
                fragment_keys.sort();

                if *is_list {
                    let j_alias = format!("j_{}_{}", node.alias, field_name);
                    let mut case_statements = Vec::new();
                    
                    for model_name in &fragment_keys {
                        let fragment_node = target_fragments.get(*model_name).unwrap();
                        let sub_obj = compile_select(fragment_node, Some((&node.alias, &format!("{}.value->>'__id'", j_alias))));
                        
                        let mut where_conds = vec![format!("{}.__id = {}.value->>'__id'", fragment_node.alias, j_alias)];
                        if let Some(filters) = &fragment_node.filters {
                            where_conds.push(compile_where_clause(filters, &fragment_node.alias));
                        }
                        let where_str = where_conds.join(" AND ");

                        case_statements.push(format!(
                            "WHEN '{}' THEN (SELECT {} FROM {} AS {} WHERE {})",
                            model_name,
                            sub_obj,
                            model_name,
                            fragment_node.alias,
                            where_str
                        ));
                    }
                    
                    let case_block = format!("CASE {}.value->>'type' {} ELSE NULL END", j_alias, case_statements.join(" "));
                    let subquery = format!(
                        "(SELECT json_group_array(json({})) FROM (SELECT value, key FROM json_each({}.{}) ORDER BY key ASC) AS {})",
                        case_block,
                        node.alias,
                        field_name,
                        j_alias
                    );
                    json_pairs.push(format!("'{}', {}", field_name, subquery));
                } else {
                    let type_col = format!("{}.{}_type", node.alias, field_name); // e.g., t0.result_type
                    let id_col = format!("{}.{}_id", node.alias, field_name);     // e.g., t0.result_id
                    
                    let mut case_statements = Vec::new();
                    
                    for model_name in &fragment_keys {
                        let fragment_node = target_fragments.get(*model_name).unwrap();
                        let sub_obj = compile_select(fragment_node, Some((&node.alias, &id_col)));
                        
                        let mut where_conds = vec![format!("{}.__id = {}", fragment_node.alias, id_col)];
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
            SelectField::SyntheticNull(field_name) => {
                json_pairs.push(format!("'{}', CASE {}.{} WHEN 1 THEN json('true') WHEN 0 THEN json('false') ELSE NULL END", field_name, node.alias, field_name));
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

        let mut order_by_clause = String::new();
        if !node.order_by.is_empty() {
            let orders: Vec<_> = node.order_by.iter().map(|(field, dir)| {
                let dir_str = match dir {
                    crate::ir::OrderDirection::Asc => "ASC",
                    crate::ir::OrderDirection::Desc => "DESC",
                };
                format!("{}.{} {}", node.alias, field, dir_str)
            }).collect();
            order_by_clause = format!(" ORDER BY {}", orders.join(", "));
        }

        // Root query: Wrap execution in a final SELECT returning a JSON array
            let source_table = match &node.source {
                QueryIrSource::Table(t) => t.clone(),
                QueryIrSource::Polymorphic { alias: _, branches } => {
                    let mut inner_branch_sqls = Vec::new();
                    for branch in branches {
                        let mut branch_selects = Vec::new();
                        for sel in &branch.selections {
                            match sel {
                                SelectField::Scalar(name) | SelectField::ScalarArray(name) | SelectField::ScalarBoolean(name) => branch_selects.push(format!("{}.{}", branch.alias, name)),
                                _ => {}
                            }
                        }
                        let b_where = branch.filters.as_ref().map(|w| format!(" WHERE {}", compile_where_clause(w, &branch.alias))).unwrap_or_default();
                        let b_table = match &branch.source { QueryIrSource::Table(t) => t.clone(), _ => panic!() };
                        inner_branch_sqls.push(format!("SELECT {} FROM {} AS {}{}", branch_selects.join(", "), b_table, branch.alias, b_where));
                    }
                    format!("(\n{}\n)", inner_branch_sqls.join("\nUNION ALL\n"))
                },
            };
            format!("SELECT json_group_array(json(root_payload)) AS payload FROM (SELECT {} AS root_payload FROM {} AS {}{}{}{}{});", json_obj, source_table, node.alias, root_where, order_by_clause, limit_clause, offset_clause)
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
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("User".to_string()),
            alias: "t0".to_string(),
            order_by: vec![],
            selections: vec![
                SelectField::Scalar("__id".to_string()),
                SelectField::Scalar("name".to_string()),
            ],
            filters: None,
            limit: None,
            offset: None,
        };
        let sql = compile_select(&query, None);
        assert_eq!(sql, "SELECT json_group_array(json(root_payload)) AS payload FROM (SELECT json_object('__id', t0.__id, 'name', t0.name) AS root_payload FROM User AS t0);");
    }

    #[test]
    fn test_compile_relation_select() {
        let child_query = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("Post".to_string()),
            alias: "t1".to_string(),
            order_by: vec![],
            selections: vec![
                SelectField::Scalar("__id".to_string()),
                SelectField::Scalar("title".to_string()),
            ],
            filters: None,
            limit: None,
            offset: None,
        };
        
        let query = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("User".to_string()),
            alias: "t0".to_string(),
            order_by: vec![],
            selections: vec![
                SelectField::Scalar("__id".to_string()),
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
            "SELECT json_group_array(json(root_payload)) AS payload FROM (SELECT json_object('__id', t0.__id, 'posts', (SELECT json_group_array(json_object('__id', t1.__id, 'title', t1.title)) FROM Post AS t1 WHERE t1.author_id = t0.__id)) AS root_payload FROM User AS t0);"
        );
    }
    
    #[test]
    fn test_compile_polymorphic_union() {
        let article_fragment = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("Article".to_string()),
            alias: "t1".to_string(),
            order_by: vec![],
            selections: vec![
                SelectField::Scalar("__id".to_string()),
                SelectField::Scalar("title".to_string()),
            ],
            filters: None,
            limit: None,
            offset: None,
        };
        
        let mut fragments = HashMap::new();
        fragments.insert("Article".to_string(), article_fragment);
        
        let query = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("User".to_string()),
            alias: "t0".to_string(),
            order_by: vec![],
            selections: vec![
                SelectField::Scalar("__id".to_string()),
                SelectField::Polymorphic {
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
            "SELECT json_group_array(json(root_payload)) AS payload FROM (SELECT json_object('__id', t0.__id, 'search', CASE t0.search_type WHEN 'Article' THEN (SELECT json_object('__id', t1.__id, 'title', t1.title) FROM Article AS t1 WHERE t1.__id = t0.search_id) ELSE NULL END) AS root_payload FROM User AS t0);"
        );
    }

    #[test]
    fn test_compile_deep_recursive_relation() {
        let comments_query = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("Comment".to_string()),
            alias: "t2".to_string(),
            order_by: vec![],
            selections: vec![
                SelectField::Scalar("__id".to_string()),
                SelectField::Scalar("body".to_string()),
            ],
            filters: None,
            limit: None,
            offset: None,
        };
        
        let posts_query = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("Post".to_string()),
            alias: "t1".to_string(),
            order_by: vec![],
            selections: vec![
                SelectField::Scalar("__id".to_string()),
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
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("User".to_string()),
            alias: "t0".to_string(),
            order_by: vec![],
            selections: vec![
                SelectField::Scalar("__id".to_string()),
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
            "SELECT json_group_array(json(root_payload)) AS payload FROM (SELECT json_object('__id', t0.__id, 'posts', (SELECT json_group_array(json_object('__id', t1.__id, 'comments', (SELECT json_group_array(json_object('__id', t2.__id, 'body', t2.body)) FROM Comment AS t2 WHERE t2.post_id = t1.__id))) FROM Post AS t1 WHERE t1.author_id = t0.__id)) AS root_payload FROM User AS t0);"
        );
    }

    #[test]
    fn test_compile_scalar_array() {
        let query = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("User".to_string()),
            alias: "t0".to_string(),
            order_by: vec![],
            selections: vec![
                SelectField::Scalar("__id".to_string()),
                SelectField::ScalarArray("tags".to_string()),
            ],
            filters: None,
            limit: None,
            offset: None,
        };
        let sql = compile_select(&query, None);
        assert_eq!(sql, "SELECT json_group_array(json(root_payload)) AS payload FROM (SELECT json_object('__id', t0.__id, 'tags', json(t0.tags)) AS root_payload FROM User AS t0);");
    }

    #[test]
    fn test_compile_single_relation() {
        let child_query = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("Profile".to_string()),
            alias: "t1".to_string(),
            order_by: vec![],
            selections: vec![
                SelectField::Scalar("bio".to_string()),
            ],
            filters: None,
            limit: None,
            offset: None,
        };
        
        let query = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("User".to_string()),
            alias: "t0".to_string(),
            order_by: vec![],
            selections: vec![
                SelectField::Scalar("__id".to_string()),
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
            "SELECT json_group_array(json(root_payload)) AS payload FROM (SELECT json_object('__id', t0.__id, 'profile', (SELECT json_object('bio', t1.bio) FROM Profile AS t1 WHERE t1.user_id = t0.__id LIMIT 1)) AS root_payload FROM User AS t0);"
        );
    }

    #[test]
    fn test_compile_multi_fragment_polymorphic_union() {
        let article_fragment = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("Article".to_string()),
            alias: "t1".to_string(),
            order_by: vec![],
            selections: vec![SelectField::Scalar("title".to_string())],
            filters: None, limit: None, offset: None,
        };
        let video_fragment = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("Video".to_string()),
            alias: "t2".to_string(),
            order_by: vec![],
            selections: vec![SelectField::Scalar("duration".to_string())],
            filters: None, limit: None, offset: None,
        };
        
        let mut fragments = HashMap::new();
        // Insert in reverse alphabetical to verify sorting
        fragments.insert("Video".to_string(), video_fragment);
        fragments.insert("Article".to_string(), article_fragment);
        
        let query = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("User".to_string()),
            alias: "t0".to_string(),
            order_by: vec![],
            selections: vec![
                SelectField::Scalar("__id".to_string()),
                SelectField::Polymorphic {
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
            "SELECT json_group_array(json(root_payload)) AS payload FROM (SELECT json_object('__id', t0.__id, 'content', CASE t0.content_type WHEN 'Article' THEN (SELECT json_object('title', t1.title) FROM Article AS t1 WHERE t1.__id = t0.content_id) WHEN 'Video' THEN (SELECT json_object('duration', t2.duration) FROM Video AS t2 WHERE t2.__id = t0.content_id) ELSE NULL END) AS root_payload FROM User AS t0);"
        );
    }

    #[test]
    fn test_compile_pagination_and_filtering() {
        let query = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("User".to_string()),
            alias: "t0".to_string(),
            order_by: vec![],
            selections: vec![SelectField::Scalar("__id".to_string())],
            filters: Some(WhereClause::Field("name".to_string(), WhereCondition::Eq("Alice".to_string()))),
            limit: Some(10),
            offset: Some(5),
        };
        let sql = compile_select(&query, None);
        assert_eq!(
            sql,
            "SELECT json_group_array(json(root_payload)) AS payload FROM (SELECT json_object('__id', t0.__id) AS root_payload FROM User AS t0 WHERE t0.name = 'Alice' LIMIT 10 OFFSET 5);"
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
        let clause1 = WhereClause::Field("__id".to_string(), WhereCondition::In(vec![]));
        assert_eq!(compile_where_clause(&clause1, "t0"), "1=0");

        let clause2 = WhereClause::Field("managerId".to_string(), WhereCondition::IsNull);
        assert_eq!(compile_where_clause(&clause2, "t1"), "t1.managerId IS NULL");

        let clause3 = WhereClause::Field("email".to_string(), WhereCondition::IsNotNull);
        assert_eq!(compile_where_clause(&clause3, "t2"), "t2.email IS NOT NULL");
    }

    #[test]
    fn test_compile_relation_with_pagination_and_filtering() {
        let child_query = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("Post".to_string()),
            alias: "t1".to_string(),
            order_by: vec![],
            selections: vec![SelectField::Scalar("title".to_string())],
            filters: Some(WhereClause::Field("published".to_string(), WhereCondition::Eq("true".to_string()))),
            limit: Some(5),
            offset: Some(2),
        };
        
        let query = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("User".to_string()),
            alias: "t0".to_string(),
            order_by: vec![],
            selections: vec![
                SelectField::Scalar("__id".to_string()),
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
            "SELECT json_group_array(json(root_payload)) AS payload FROM (SELECT json_object('__id', t0.__id, 'posts', (SELECT json_group_array(json_object('title', t1.title)) FROM Post AS t1 WHERE t1.author_id = t0.__id AND t1.published = 'true' LIMIT 5 OFFSET 2)) AS root_payload FROM User AS t0);"
        );
    }

    #[test]
    fn test_compile_polymorphic_union_with_filtering() {
        let article_fragment = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("Article".to_string()),
            alias: "t1".to_string(),
            order_by: vec![],
            selections: vec![SelectField::Scalar("title".to_string())],
            filters: Some(WhereClause::Field("status".to_string(), WhereCondition::Eq("published".to_string()))),
            limit: None, offset: None,
        };
        
        let mut fragments = HashMap::new();
        fragments.insert("Article".to_string(), article_fragment);
        
        let query = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("User".to_string()),
            alias: "t0".to_string(),
            order_by: vec![],
            selections: vec![
                SelectField::Scalar("__id".to_string()),
                SelectField::Polymorphic {
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
            "SELECT json_group_array(json(root_payload)) AS payload FROM (SELECT json_object('__id', t0.__id, 'search', CASE t0.search_type WHEN 'Article' THEN (SELECT json_object('title', t1.title) FROM Article AS t1 WHERE t1.__id = t0.search_id AND t1.status = 'published') ELSE NULL END) AS root_payload FROM User AS t0);"
        );
    }

    #[test]
    fn test_compile_polymorphic_base_singular() {
        let article_fragment = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("Article".to_string()),
            alias: "t1".to_string(),
            order_by: vec![],
            selections: vec![SelectField::Scalar("title".to_string())],
            filters: None, limit: None, offset: None,
        };

        let video_fragment = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("Video".to_string()),
            alias: "t2".to_string(),
            order_by: vec![],
            selections: vec![SelectField::Scalar("duration".to_string())],
            filters: None, limit: None, offset: None,
        };

        let mut fragments = HashMap::new();
        fragments.insert("Article".to_string(), article_fragment);
        fragments.insert("Video".to_string(), video_fragment);

        let query = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("Comment".to_string()),
            alias: "t0".to_string(),
            order_by: vec![],
            selections: vec![
                SelectField::Scalar("__id".to_string()),
                SelectField::Polymorphic {
                    field_name: "parent".to_string(),
                    is_list: false,
                    target_fragments: fragments,
                }
            ],
            filters: None, limit: None, offset: None,
        };

        let sql = compile_select(&query, None);
        
        // Assert the discriminator columns 'parent_type' and 'parent_id' are utilized correctly
        assert!(sql.contains("CASE t0.parent_type"));
        assert!(sql.contains("WHEN 'Article' THEN (SELECT json_object('title', t1.title) FROM Article AS t1 WHERE t1.__id = t0.parent_id)"));
        assert!(sql.contains("WHEN 'Video' THEN (SELECT json_object('duration', t2.duration) FROM Video AS t2 WHERE t2.__id = t0.parent_id)"));
    }

    #[test]
    fn test_compile_polymorphic_base_array() {
        let article_fragment = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("Article".to_string()),
            alias: "t1".to_string(),
            order_by: vec![],
            selections: vec![SelectField::Scalar("title".to_string())],
            filters: None, limit: None, offset: None,
        };

        let mut fragments = HashMap::new();
        fragments.insert("Article".to_string(), article_fragment);

        let query = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("User".to_string()),
            alias: "t0".to_string(),
            order_by: vec![],
            selections: vec![
                SelectField::Scalar("__id".to_string()),
                SelectField::Polymorphic {
                    field_name: "favorites".to_string(),
                    is_list: true,
                    target_fragments: fragments,
                }
            ],
            filters: None, limit: None, offset: None,
        };

        let sql = compile_select(&query, None);
        println!("POLYMORPHIC BASE ARRAY SQL:\n{}", sql);
        
        // Asserts unpacking of the JSON array column 'favorites'
        assert!(sql.contains("json_each(t0.favorites) ORDER BY key ASC) AS j_t0_favorites"));
        assert!(sql.contains("CASE j_t0_favorites.value->>'type'"));
        assert!(sql.contains("WHEN 'Article' THEN (SELECT json_object('title', t1.title) FROM Article AS t1 WHERE t1.__id = j_t0_favorites.value->>'__id')"));
    }

    #[test]
    fn test_compile_polymorphic_union_array() {
        let article_fragment = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("Article".to_string()),
            alias: "t1".to_string(),
            order_by: vec![],
            selections: vec![SelectField::Scalar("title".to_string())],
            filters: Some(WhereClause::Field("status".to_string(), WhereCondition::Eq("published".to_string()))),
            limit: None,
            offset: None,
        };
        
        let mut fragments = HashMap::new();
        fragments.insert("Article".to_string(), article_fragment);
        
        let query = QueryNode {
            primary_key: "__id".to_string(),
            source: QueryIrSource::Table("User".to_string()),
            alias: "t0".to_string(),
            order_by: vec![],
            selections: vec![
                SelectField::Scalar("__id".to_string()),
                SelectField::Polymorphic {
                    field_name: "contents".to_string(),
                    is_list: true,
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
            "SELECT json_group_array(json(root_payload)) AS payload FROM (SELECT json_object('__id', t0.__id, 'contents', (SELECT json_group_array(json(CASE j_t0_contents.value->>'type' WHEN 'Article' THEN (SELECT json_object('title', t1.title) FROM Article AS t1 WHERE t1.__id = j_t0_contents.value->>'__id' AND t1.status = 'published') ELSE NULL END)) FROM (SELECT value, key FROM json_each(t0.contents) ORDER BY key ASC) AS j_t0_contents)) AS root_payload FROM User AS t0);"
        );
    }
}

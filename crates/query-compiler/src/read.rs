use crate::ir::{QueryNode, SelectField};

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
            SelectField::Relation { field_name, foreign_key, is_list, query } => {
                // The Recursive N+1 Neutralizer: Correlated Subquery with JSON aggregation
                let child_json_obj = compile_select(query, Some((&node.alias, foreign_key)));
                
                let subquery = if *is_list {
                    format!("(SELECT json_group_array({}) FROM {} AS {} WHERE {}.{} = {}.id)",
                        child_json_obj, query.target_model, query.alias, query.alias, foreign_key, node.alias)
                } else {
                    format!("(SELECT {} FROM {} AS {} WHERE {}.{} = {}.id LIMIT 1)",
                        child_json_obj, query.target_model, query.alias, query.alias, foreign_key, node.alias)
                };
                
                json_pairs.push(format!("'{}', {}", field_name, subquery));
            },
            SelectField::PolymorphicUnion { field_name, target_fragments } => {
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
                    case_statements.push(format!(
                        "WHEN '{}' THEN (SELECT {} FROM {} AS {} WHERE {}.id = {})",
                        model_name,          // e.g., 'Article'
                        sub_obj,             // e.g., json_object('title', t1.title, '__typename', 'Article')
                        model_name,          // Target table
                        fragment_node.alias, // Child table alias
                        fragment_node.alias, id_col
                    ));
                }
                
                // Generates: 'result', CASE t0.result_type WHEN 'Article' THEN (...) ELSE NULL END
                json_pairs.push(format!("'{}', CASE {} {} ELSE NULL END", field_name, type_col, case_statements.join(" ")));
            }
        }
    }

    let json_obj = format!("json_object({})", json_pairs.join(", "));
    
    if parent_ref.is_none() {
        // Root query: Wrap execution in a final SELECT returning a JSON array
        format!("SELECT json_group_array({}) AS payload FROM {} AS {};", json_obj, node.target_model, node.alias)
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
                    query: Box::new(child_query),
                }
            ],
            filters: None,
            limit: None,
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
                    target_fragments: fragments,
                }
            ],
            filters: None,
            limit: None,
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
                    query: Box::new(comments_query),
                }
            ],
            filters: None,
            limit: None,
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
                    query: Box::new(posts_query),
                }
            ],
            filters: None,
            limit: None,
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
                    query: Box::new(child_query),
                }
            ],
            filters: None,
            limit: None,
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
            filters: None, limit: None,
        };
        let video_fragment = QueryNode {
            target_model: "Video".to_string(),
            alias: "t2".to_string(),
            selections: vec![SelectField::Scalar("duration".to_string())],
            filters: None, limit: None,
        };
        
        let mut fragments = HashMap::new();
        // Insert in reverse alphabetical to verify sorting
        fragments.insert("Video".to_string(), video_fragment);
        fragments.insert("Article".to_string(), article_fragment);
        
        let query = QueryNode {
            target_model: "User".to_string(),
            alias: "t0".to_string(),
            selections: vec![
                SelectField::PolymorphicUnion {
                    field_name: "content".to_string(),
                    target_fragments: fragments,
                }
            ],
            filters: None,
            limit: None,
        };
        let sql = compile_select(&query, None);
        
        // Assert sorting: Article (t1) should come before Video (t2)
        assert!(sql.contains("WHEN 'Article' THEN (SELECT json_object('title', t1.title) FROM Article AS t1 WHERE t1.id = t0.content_id) WHEN 'Video' THEN (SELECT json_object('duration', t2.duration) FROM Video AS t2 WHERE t2.id = t0.content_id)"));
    }
}

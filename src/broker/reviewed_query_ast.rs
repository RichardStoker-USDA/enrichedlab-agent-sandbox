//! Parameter nodes become typed literal nodes. No parameter value is parsed as SQL.
use super::*;
use pg_query::protobuf::{self, node::Node as Kind};

fn literal(value: &Value, sql_type: &str) -> protobuf::Node {
    let content = value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string());
    protobuf::Node {
        node: Some(Kind::TypeCast(Box::new(protobuf::TypeCast {
            arg: Some(Box::new(protobuf::Node {
                node: Some(Kind::AConst(protobuf::AConst {
                    val: Some(protobuf::a_const::Val::Sval(protobuf::String {
                        sval: content,
                    })),
                    location: -1,
                    ..Default::default()
                })),
            })),
            type_name: Some(protobuf::TypeName {
                names: ["pg_catalog", sql_type]
                    .into_iter()
                    .map(|s| protobuf::Node {
                        node: Some(Kind::String(protobuf::String { sval: s.into() })),
                    })
                    .collect(),
                typemod: -1,
                location: -1,
                ..Default::default()
            }),
            location: -1,
        }))),
    }
}

/// Offsets are source spelling, not AST semantics; deparsing necessarily changes them.
fn semantic(tree: &mut Value) {
    match tree {
        Value::Object(map) => {
            for key in ["location", "stmt_location", "stmt_len"] {
                map.remove(key);
            }
            for child in map.values_mut() {
                semantic(child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(semantic),
        _ => {}
    }
}

pub(super) fn lower(
    template: &str,
    parameters: &[Parameter],
    normalized: &Value,
) -> Result<String, Fault> {
    let mut parsed =
        pg_query::parse(template).map_err(|_| fault("reviewed_query_template_parse"))?;
    if parsed.protobuf.stmts.len() != 1 {
        return Err(fault("reviewed_query_template_statements"));
    }
    let root = parsed.protobuf.stmts[0]
        .stmt
        .as_mut()
        .ok_or_else(|| fault("reviewed_query_template_not_select"))?;
    if !matches!(root.node, Some(Kind::SelectStmt(_))) {
        return Err(fault("reviewed_query_template_not_select"));
    }
    let values: Vec<protobuf::Node> = parameters
        .iter()
        .map(|p| literal(&normalized[&p.name], p.sql_type()))
        .collect();
    let mut used = vec![false; parameters.len()];
    visit(root, &values, &mut used, 0)?;
    if used.contains(&false) {
        return Err(fault("reviewed_query_placeholders_invalid"));
    }
    let mut tree = serde_json::to_value(&parsed.protobuf)
        .map_err(|_| fault("reviewed_query_template_parse"))?;
    if has_parameter(&tree) {
        return Err(fault("reviewed_query_placeholders_invalid"));
    }
    let sql = pg_query::deparse(&parsed.protobuf).map_err(|_| fault("reviewed_query_roundtrip"))?;
    let reparsed = pg_query::parse(&sql).map_err(|_| fault("reviewed_query_roundtrip"))?;
    let mut again =
        serde_json::to_value(reparsed.protobuf).map_err(|_| fault("reviewed_query_roundtrip"))?;
    semantic(&mut tree);
    semantic(&mut again);
    if tree != again {
        return Err(fault("reviewed_query_roundtrip"));
    }
    Ok(sql)
}

fn has_parameter(v: &Value) -> bool {
    match v {
        Value::Object(m) => m.contains_key("ParamRef") || m.values().any(has_parameter),
        Value::Array(a) => a.iter().any(has_parameter),
        _ => false,
    }
}

#[path = "reviewed_query_visit.rs"]
mod visitor;
use visitor::visit;

//! SELECT, window and aggregate fields in typed parameter substitution.
use super::*;

pub(super) fn visit_selectstmt(
    n: &mut protobuf::SelectStmt,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    for child in &mut n.distinct_clause {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.target_list {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.from_clause {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.where_clause {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.group_clause {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.having_clause {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.window_clause {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.values_lists {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.sort_clause {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.limit_offset {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.limit_count {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.locking_clause {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.with_clause {
        visit_withclause(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.larg {
        visit_selectstmt(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.rarg {
        visit_selectstmt(child, values, used, depth + 1)?;
    }
    Ok(())
}

pub(super) fn visit_funccall(
    n: &mut protobuf::FuncCall,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    for child in &mut n.funcname {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.args {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.agg_order {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.agg_filter {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.over {
        visit_windowdef(child, values, used, depth + 1)?;
    }
    Ok(())
}

pub(super) fn visit_windowdef(
    n: &mut protobuf::WindowDef,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    for child in &mut n.partition_clause {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.order_clause {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.start_offset {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.end_offset {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

pub(super) fn visit_withclause(
    n: &mut protobuf::WithClause,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    for child in &mut n.ctes {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

//! Typed traversal of the SELECT expression grammar. Any unvisited parameter is refused before deparsing.
use super::*;
#[path = "reviewed_query_select.rs"]
mod select;
use select::*;
pub(super) fn visit(
    n: &mut protobuf::Node,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    match &mut n.node {
        Some(Kind::ParamRef(p)) => {
            let index = usize::try_from(p.number)
                .ok()
                .and_then(|n| n.checked_sub(1))
                .filter(|n| *n < values.len())
                .ok_or_else(|| fault("reviewed_query_placeholders_invalid"))?;
            *n = values[index].clone();
            used[index] = true;
            Ok(())
        }
        Some(Kind::SelectStmt(n)) => visit_selectstmt(n, values, used, depth),
        Some(Kind::ResTarget(n)) => visit_restarget(n, values, used, depth),
        Some(Kind::ColumnRef(n)) => visit_columnref(n, values, used, depth),
        Some(Kind::AExpr(n)) => visit_aexpr(n, values, used, depth),
        Some(Kind::BoolExpr(n)) => visit_boolexpr(n, values, used, depth),
        Some(Kind::NullTest(n)) => visit_nulltest(n, values, used, depth),
        Some(Kind::BooleanTest(n)) => visit_booleantest(n, values, used, depth),
        Some(Kind::FuncCall(n)) => visit_funccall(n, values, used, depth),
        Some(Kind::TypeCast(n)) => visit_typecast(n, values, used, depth),
        Some(Kind::TypeName(n)) => visit_typename(n, values, used, depth),
        Some(Kind::RangeVar(n)) => visit_rangevar(n, values, used, depth),
        Some(Kind::JoinExpr(n)) => visit_joinexpr(n, values, used, depth),
        Some(Kind::SortBy(n)) => visit_sortby(n, values, used, depth),
        Some(Kind::CommonTableExpr(n)) => visit_commontableexpr(n, values, used, depth),
        Some(Kind::SubLink(n)) => visit_sublink(n, values, used, depth),
        Some(Kind::CaseExpr(n)) => visit_caseexpr(n, values, used, depth),
        Some(Kind::CaseWhen(n)) => visit_casewhen(n, values, used, depth),
        Some(Kind::CoalesceExpr(n)) => visit_coalesceexpr(n, values, used, depth),
        Some(Kind::MinMaxExpr(n)) => visit_minmaxexpr(n, values, used, depth),
        Some(Kind::RangeSubselect(n)) => visit_rangesubselect(n, values, used, depth),
        Some(Kind::List(n)) => visit_list(n, values, used, depth),
        Some(Kind::WindowDef(n)) => visit_windowdef(n, values, used, depth),
        Some(Kind::AIndirection(n)) => visit_aindirection(n, values, used, depth),
        Some(Kind::AIndices(n)) => visit_aindices(n, values, used, depth),
        Some(Kind::RowExpr(n)) => visit_rowexpr(n, values, used, depth),
        Some(Kind::CollateClause(n)) => visit_collateclause(n, values, used, depth),
        Some(Kind::GroupingSet(n)) => visit_groupingset(n, values, used, depth),
        Some(Kind::Alias(n)) => visit_alias(n, values, used, depth),
        _ => Ok(()),
    }
}

fn visit_restarget(
    n: &mut protobuf::ResTarget,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    for child in &mut n.indirection {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.val {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_columnref(
    n: &mut protobuf::ColumnRef,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    for child in &mut n.fields {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_aexpr(
    n: &mut protobuf::AExpr,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    for child in &mut n.name {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.lexpr {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.rexpr {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_boolexpr(
    n: &mut protobuf::BoolExpr,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    if let Some(child) = &mut n.xpr {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.args {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_nulltest(
    n: &mut protobuf::NullTest,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    if let Some(child) = &mut n.xpr {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.arg {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_booleantest(
    n: &mut protobuf::BooleanTest,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    if let Some(child) = &mut n.xpr {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.arg {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_typecast(
    n: &mut protobuf::TypeCast,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    if let Some(child) = &mut n.arg {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.type_name {
        visit_typename(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_typename(
    n: &mut protobuf::TypeName,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    for child in &mut n.names {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.typmods {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.array_bounds {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_rangevar(
    n: &mut protobuf::RangeVar,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    if let Some(child) = &mut n.alias {
        visit_alias(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_joinexpr(
    n: &mut protobuf::JoinExpr,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    if let Some(child) = &mut n.larg {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.rarg {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.using_clause {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.join_using_alias {
        visit_alias(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.quals {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.alias {
        visit_alias(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_sortby(
    n: &mut protobuf::SortBy,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    if let Some(child) = &mut n.node {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.use_op {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_commontableexpr(
    n: &mut protobuf::CommonTableExpr,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    for child in &mut n.aliascolnames {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.ctequery {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.ctecolnames {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.ctecoltypes {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.ctecoltypmods {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.ctecolcollations {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_sublink(
    n: &mut protobuf::SubLink,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    if let Some(child) = &mut n.xpr {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.testexpr {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.oper_name {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.subselect {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_caseexpr(
    n: &mut protobuf::CaseExpr,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    if let Some(child) = &mut n.xpr {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.arg {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.args {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.defresult {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_casewhen(
    n: &mut protobuf::CaseWhen,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    if let Some(child) = &mut n.xpr {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.expr {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.result {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_coalesceexpr(
    n: &mut protobuf::CoalesceExpr,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    if let Some(child) = &mut n.xpr {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.args {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_minmaxexpr(
    n: &mut protobuf::MinMaxExpr,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    if let Some(child) = &mut n.xpr {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.args {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_rangesubselect(
    n: &mut protobuf::RangeSubselect,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    if let Some(child) = &mut n.subquery {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.alias {
        visit_alias(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_list(
    n: &mut protobuf::List,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    for child in &mut n.items {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_aindirection(
    n: &mut protobuf::AIndirection,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    if let Some(child) = &mut n.arg {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.indirection {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_aindices(
    n: &mut protobuf::AIndices,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    if let Some(child) = &mut n.lidx {
        visit(child, values, used, depth + 1)?;
    }
    if let Some(child) = &mut n.uidx {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_rowexpr(
    n: &mut protobuf::RowExpr,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    if let Some(child) = &mut n.xpr {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.args {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.colnames {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_collateclause(
    n: &mut protobuf::CollateClause,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    if let Some(child) = &mut n.arg {
        visit(child, values, used, depth + 1)?;
    }
    for child in &mut n.collname {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_groupingset(
    n: &mut protobuf::GroupingSet,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    for child in &mut n.content {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

fn visit_alias(
    n: &mut protobuf::Alias,
    values: &[protobuf::Node],
    used: &mut [bool],
    depth: usize,
) -> Result<(), Fault> {
    if depth > 128 {
        return Err(fault("reviewed_query_template_depth"));
    }
    for child in &mut n.colnames {
        visit(child, values, used, depth + 1)?;
    }
    Ok(())
}

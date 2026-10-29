//! Admission at the worker boundary, before any source connection.
use super::*;

pub(super) fn admit_stored(query: &TaskQuery) -> Result<sql_guard::Admitted, Stop> {
    // The worker admits the query again from the lead's text and the source's lists; the stored text must be the result.
    let relations: BTreeSet<String> = query.relations.iter().cloned().collect();
    let functions: BTreeSet<String> = query.functions.iter().cloned().collect();
    let allowed = Allowed {
        schema: &query.schema,
        relations: &relations,
        functions: &functions,
        row_cap: query.row_cap,
    };
    if let Some(record) = &query.reviewed_query {
        record
            .check(&query.admitted_sql, &allowed)
            .map_err(|fault| refused(fault.code))?;
    }
    let allowed = Allowed {
        row_cap: query
            .reviewed_query
            .as_ref()
            .map_or(query.row_cap, |r| r.row_cap),
        ..allowed
    };
    let admitted = sql_guard::admit(&query.proposed_sql, &allowed).map_err(|r| refused(r.code))?;
    if query.guard != sql_guard::GUARD
        || admitted.sql != query.admitted_sql
        || admitted.limit != query.row_limit
    {
        return Err(refused("guard_mismatch"));
    }
    Ok(admitted)
}

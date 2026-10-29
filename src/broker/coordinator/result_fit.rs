//! A finished template-analysis result as one lead call can hold it (template-analysis results
//! only; any other result enters whole or is left out). A result's view enters the context whole when it
//! fits. Otherwise its tables' cells are withheld one table at a time, the table whose cells take the most
//! bytes first, each marked `cells_omitted: context_budget`, until the view fits. Only a result that does not fit with
//! every table's cells withheld is left out (`results_omitted`). The lead keeps each table's path, rows and columns, so it
//! can give a table whole as an answer table (the workspace shows its rows from the retained result), while findings and
//! table statements bind only to cells the call showed.
//!
//! At first a result entered whole or not at all. With the lead's 65,536-byte ceiling less its 24,864-byte system text
//! a call holds about 34 KB of context, and the reviewed FoodData Central normalization's 18-column per-food table with
//! its cells did not fit: the call left the result out, the lead named its table from an earlier call, and the reply was
//! refused as unbound.
use serde_json::{Value, json};
use std::cmp::Reverse;

/// Why a shown table's cells were left out of this call.
pub(super) const CONTEXT_BUDGET: &str = "context_budget";

/// `shown` within `room` bytes, its largest tables' cells withheld as needed; none when even its tables without cells do
/// not fit.
pub(super) fn fit(mut shown: Value, room: usize) -> Option<Value> {
    let size = |v: &Value| serde_json::to_vec(v).map_or(usize::MAX, |b| b.len());
    loop {
        if size(&shown) <= room {
            return Some(shown);
        }
        // The table whose cells take the most bytes; on a tie, the first.
        let largest = shown["tables"]
            .as_array()?
            .iter()
            .enumerate()
            .filter(|(_, t)| t.get("cells").is_some())
            .max_by_key(|(n, t)| (size(&t["cells"]), Reverse(*n)))
            .map(|(n, _)| n)?;
        let table = shown["tables"][largest].as_object_mut()?;
        table.remove("cells");
        table.remove("rows_shown");
        table.insert("cells_omitted".into(), json!(CONTEXT_BUDGET));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> Value {
        let row = |n: usize, width: usize| {
            let mut cells = serde_json::Map::new();
            cells.insert("#".into(), json!(n));
            for c in 0..width {
                cells.insert(format!("value_{c:02}_per_portion"), json!(format!("{c}.5")));
            }
            Value::Object(cells)
        };
        json!({"ref": "artifact://tasks/t/a", "label": "normalized", "status": "summarized", "tables": [
            {"table": "results/results.csv", "rows": 5, "columns": ["value_00_per_portion"],
             "cells": (1..=5).map(|n| row(n, 18)).collect::<Vec<_>>()},
            {"table": "results/statistics.csv", "rows": 3, "columns": ["n", "mean"],
             "cells": (1..=3).map(|n| row(n, 2)).collect::<Vec<_>>()},
            {"table": "results/long.csv", "rows": 900, "columns": ["food"], "cells_omitted": "too_many_rows"}]})
    }

    fn bytes(v: &Value) -> usize {
        serde_json::to_vec(v).unwrap().len()
    }

    #[test]
    fn a_result_that_fits_enters_whole() {
        let whole = view();
        assert_eq!(fit(whole.clone(), bytes(&whole)), Some(whole));
    }

    #[test]
    fn the_largest_tables_cells_go_first_and_the_rest_stay() {
        let whole = view();
        let fitted = fit(whole.clone(), bytes(&whole) - 1).unwrap();
        let tables = fitted["tables"].as_array().unwrap();
        assert_eq!(tables[0]["cells_omitted"], CONTEXT_BUDGET);
        assert!(tables[0].get("cells").is_none());
        // The small statistics table keeps its cells for findings; an earlier reason stays as it was.
        assert_eq!(tables[1]["cells"].as_array().unwrap().len(), 3);
        assert_eq!(tables[2]["cells_omitted"], "too_many_rows");
        // Paths, rows and columns stay, so the table can still be given whole.
        assert_eq!(
            (&tables[0]["table"], &tables[0]["rows"]),
            (&json!("results/results.csv"), &json!(5))
        );
        assert!(bytes(&fitted) < bytes(&whole) - 1);
    }

    #[test]
    fn a_result_without_room_even_for_its_tables_is_left_out() {
        let whole = view();
        let mut compact = whole.clone();
        for t in compact["tables"].as_array_mut().unwrap() {
            if t.get("cells").is_some() {
                let t = t.as_object_mut().unwrap();
                t.remove("cells");
                t.insert("cells_omitted".into(), json!(CONTEXT_BUDGET));
            }
        }
        assert_eq!(fit(whole.clone(), bytes(&compact)), Some(compact.clone()));
        assert_eq!(fit(whole.clone(), bytes(&compact) - 1), None);
        // A result with no tables, such as a failed step's status, is all or nothing.
        let status = json!({"ref": "artifact://tasks/t/b", "status": "failed"});
        assert_eq!(fit(status.clone(), bytes(&status) - 1), None);
    }

    #[test]
    fn rows_shown_goes_with_the_cells() {
        let mut whole = view();
        whole["tables"][0]["rows_shown"] = json!(50);
        let fitted = fit(whole.clone(), bytes(&whole) - 1).unwrap();
        assert!(fitted["tables"][0].get("rows_shown").is_none());
    }
}

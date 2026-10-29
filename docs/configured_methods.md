# Configured methods

A configured method runs a platform recipe with settings the research lead chooses. The lead writes no code: it picks
one listed choice for each setting, and the broker fills the recipe's slots with the reviewed R text of those choices.


## Files

Each method lives in `profiles/configured/<name>/`:

- `block.R`: the recipe, an `analyze <- function(data)` block for template `r-generated-analysis/v2`, with `{{slot}}`
  markers. It follows the same rules as any admitted block (allowlisted names only, no data literals).
- `configuration.json`: schema `enrichedlab.configured-block.v1` and `settings`. Each setting has a `label` and
  `choices`; each choice has a `label` and `slots`, the exact R text for every slot it fills. Every choice of a setting
  fills the same slots, each marked slot is filled by exactly one setting, and slot text is one line without `{{` or
  `}}`.

`src/broker/configured.rs` parses and checks a configuration, renders a block from chosen settings and builds the
contract's `configuration` object (`contract_configuration`). Its tests render every combination of every setting and
admit each result; add the new method's files to those tests.

Admission is not enough: the template runs the block in an environment that holds only allowlisted names, and some
allowlisted functions need helpers that are not there (`range` calls `range.default`). Before enrolling a recipe, run
every combination through the real template with `scripts/check_configured_recipe.py <recipe dir> <template.R>
<contract.dcf> <dataset csv>`; it requires `succeeded` and a passed input-dependence rerun for each. Recipe
`bootstrap-interval-v1` drew its figure axis with `range` and failed there on every combination; `bootstrap-interval-v2`
uses `min` and `max`. Template rows are immutable, so v1 stays as the record of its retired profile.

## Catalog

The method is a profile with a `broker_profile_templates` row on the same template and site profile as model-written
R. The row's contract holds `dataset.columns` (for admission) and `configuration`: the block text, its SHA-256 and the
settings. Template rows are immutable, so a changed recipe needs a new profile key. The profile also needs a grant, an
entry in the catalog Secret, custody routes and output bindings, a worker binding and a starting-point entry, the same
as a model-written template profile.

## Checks

- The coordinator refuses code for a configured method and settings for any other (`proposal_code_unexpected`,
  `proposal_settings_invalid`); a missing or unknown choice is `proposal_settings_missing` or
  `proposal_settings_invalid`.
- The rendered block passes the same admission as model-written code; if it does not, the turn stops with
  `configured_block_rejected`.
- The turn commit (migration 022) renders the block again from the template row and refuses any difference, so a
  configured task can only hold its recipe. It stores the configuration with each setting's label and the chosen
  choice's label from the template row.
- Lineage classes the task `configured` and its outputs `configured_tool_output`; the review state stays unvalidated.

The workspace names such an analysis a configured recipe, lists the chosen settings and never calls its code
model-written. Results are unvalidated until a person reviews them, and the recipe itself still needs an independent
review before the product calls it reviewed.

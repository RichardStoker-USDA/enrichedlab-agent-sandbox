# Nutrient profile of one Foundation Food, version 1

Registration request for `fdc-foundation-2026-04`. Lists `food_nutrient` rows joined to `nutrient` for one FDC id
that is a Foundation Food in the release (an id of a sample or sub-sample food returns no rows). Amounts are per 100 g
as USDA publishes them; `data_points`, `min`, `max` and `median` are passed through unchanged (null where unpublished).
Ordering follows the release's `nutrient.rank`, then the nutrient number and name in byte order and the row id.

The release publishes two amounts for one food and nutrient in four cases (nutrients 1106 and 2066). Both rows are
listed; duplicates are not merged.

Checked as `fdc_foundation_2026_04_reader` (counts and column types only): the first Foundation
Food returned 121 rows with 121 distinct nutrients, all with an amount; a sub-sample food's id returned none. Output
types: integer, text, text, text, numeric, integer, numeric, numeric, numeric.

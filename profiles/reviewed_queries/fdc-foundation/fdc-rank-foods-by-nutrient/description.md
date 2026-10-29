# Rank Foundation Foods by one nutrient, version 1

Registration request for `fdc-foundation-2026-04`. Ranks Foundation Foods by `food_nutrient.amount` (per 100 g) for one
nutrient id, highest or lowest first (closed choice, default highest), with a typed limit of 1 to 100 (default 20) and
the guard's fixed LIMIT 100 on the outer SELECT. Rows without an amount are left out. Ties order by description in
byte order, FDC id and row id, so selection at the limit is stable.

The release publishes two amounts for one food and nutrient in four cases (nutrients 1106 and 2066); both rows are
ranked, so such a food can appear twice. Nutrient ids come from `fdc-food-nutrients`.

Checked as `fdc_foundation_2026_04_reader` (counts, ordering flags and column types only): protein
(1003) highest returned 20 distinct foods in descending order; vitamin C (1162) lowest returned 10 rows in ascending
order. Output types: integer, text, text, text, text, numeric.

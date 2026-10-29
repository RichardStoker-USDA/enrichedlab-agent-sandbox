# Find Foundation Foods by description, version 1

Registration request for `fdc-foundation-2026-04` (FoodData Central Foundation Foods, April 2026). The template uses
unqualified relation names, so the same text binds to another Foundation release's source, whose schema the read
transaction sets as its search path.

The phrase is matched with `ILIKE '%' || phrase || '%'` against `food.description`, restricted to the release's
`foundation_food` rows, so sample, sub-sample and acquisition foods are left out. SQL wildcards `%` and `_` keep their
meaning. Ordering: an exact case-insensitive match first, then shorter descriptions, then the description in byte order
and the FDC id, so selection at the limit is stable. A row-number predicate applies the typed limit (1 to 100,
default 25) while the outer SELECT keeps the guard's fixed LIMIT 100. Empty text lists every Foundation Food.

Checked as `fdc_foundation_2026_04_reader` in a read-only transaction (counts and column types
only): a common phrase returned its matches, empty text returned 100 of the 395 Foundation Foods, a phrase with no
match returned none, and a food's own description put that food first. Output types: integer, text, text, text.

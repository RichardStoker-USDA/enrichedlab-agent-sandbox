WITH ranked AS (
    SELECT f.fdc_id, k.ndb_number, f.description, c.description AS food_category, n.name AS nutrient,
           n.unit_name AS unit, fn.amount,
           row_number() OVER (
               ORDER BY CASE WHEN $2 = 'lowest' THEN fn.amount END ASC NULLS LAST,
                        CASE WHEN $2 = 'highest' THEN fn.amount END DESC NULLS LAST,
                        f.description COLLATE "C" ASC, f.fdc_id ASC, fn.id ASC
           ) AS ordinal
    FROM food_nutrient fn
    JOIN sr_legacy_food k ON k.fdc_id = fn.fdc_id
    JOIN food f ON f.fdc_id = fn.fdc_id
    JOIN nutrient n ON n.id = fn.nutrient_id
    LEFT JOIN food_category c ON c.id = f.food_category_id
    WHERE fn.nutrient_id = $1 AND fn.amount IS NOT NULL
)
SELECT fdc_id, ndb_number, description, food_category, nutrient, unit, amount
FROM ranked
WHERE ordinal <= $3
ORDER BY ordinal
LIMIT 100

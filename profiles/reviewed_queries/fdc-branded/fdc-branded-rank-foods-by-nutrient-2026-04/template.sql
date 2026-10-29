WITH ranked AS (
    SELECT f.fdc_id, f.description, k.brand_owner, k.brand_name, n.name AS nutrient,
           n.unit_name AS unit, fn.amount,
           row_number() OVER (
               ORDER BY CASE WHEN $2 = 'lowest' THEN fn.amount END ASC NULLS LAST,
                        CASE WHEN $2 = 'highest' THEN fn.amount END DESC NULLS LAST,
                        f.description COLLATE "C" ASC, f.fdc_id ASC, fn.id ASC
           ) AS ordinal
    FROM food_nutrient fn
    JOIN branded_food k ON k.fdc_id = fn.fdc_id
    JOIN food f ON f.fdc_id = fn.fdc_id
    JOIN nutrient n ON n.id = fn.nutrient_id
    WHERE fn.nutrient_id = $1 AND fn.amount IS NOT NULL
)
SELECT fdc_id, description, brand_owner, brand_name, nutrient, unit, amount
FROM ranked
WHERE ordinal <= $3
ORDER BY ordinal
LIMIT 100

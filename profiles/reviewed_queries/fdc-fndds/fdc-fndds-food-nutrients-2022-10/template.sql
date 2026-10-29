SELECT n.id AS nutrient_id, n.nutrient_nbr, n.name AS nutrient, n.unit_name AS unit, fn.amount,
       fn.data_points, fn.min AS min_amount, fn.max AS max_amount, fn.median AS median_amount
FROM food_nutrient fn
JOIN nutrient n ON n.nutrient_nbr = fn.nutrient_id::text
JOIN survey_fndds_food k ON k.fdc_id = fn.fdc_id
WHERE fn.fdc_id = $1
ORDER BY n.rank ASC NULLS LAST, n.nutrient_nbr COLLATE "C" ASC NULLS LAST, n.name COLLATE "C" ASC, fn.id ASC
LIMIT 500

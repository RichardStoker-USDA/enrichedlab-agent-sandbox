WITH matched AS (
    SELECT f.fdc_id, k.food_code, f.description, w.wweia_food_category_description AS wweia_category, f.publication_date,
           row_number() OVER (
               ORDER BY (lower(f.description) = lower($1)) DESC, length(f.description) ASC,
                        f.description COLLATE "C" ASC, f.fdc_id ASC
           ) AS ordinal
    FROM food f
    JOIN survey_fndds_food k ON k.fdc_id = f.fdc_id
    LEFT JOIN wweia_food_category w ON w.wweia_food_category = k.wweia_category_number
    WHERE f.description ILIKE '%' || $1 || '%'
)
SELECT fdc_id, food_code, description, wweia_category, publication_date
FROM matched
WHERE ordinal <= $2
ORDER BY ordinal
LIMIT 100

WITH matched AS (
    SELECT f.fdc_id, f.description, c.description AS food_category, f.publication_date,
           row_number() OVER (
               ORDER BY (lower(f.description) = lower($1)) DESC, length(f.description) ASC,
                        f.description COLLATE "C" ASC, f.fdc_id ASC
           ) AS ordinal
    FROM food f
    JOIN foundation_food ff ON ff.fdc_id = f.fdc_id
    LEFT JOIN food_category c ON c.id = f.food_category_id
    WHERE f.description ILIKE '%' || $1 || '%'
)
SELECT fdc_id, description, food_category, publication_date
FROM matched
WHERE ordinal <= $2
ORDER BY ordinal
LIMIT 100

WITH matched AS (
    SELECT f.fdc_id, f.description, k.brand_owner, k.brand_name, k.gtin_upc, k.branded_food_category, f.publication_date,
           row_number() OVER (
               ORDER BY (lower(f.description) = lower($1)) DESC, length(f.description) ASC,
                        f.description COLLATE "C" ASC, f.fdc_id ASC
           ) AS ordinal
    FROM food f
    JOIN branded_food k ON k.fdc_id = f.fdc_id
    WHERE (f.description ILIKE '%' || $1 || '%' OR k.brand_name ILIKE '%' || $1 || '%'
           OR k.brand_owner ILIKE '%' || $1 || '%')
)
SELECT fdc_id, description, brand_owner, brand_name, gtin_upc, branded_food_category, publication_date
FROM matched
WHERE ordinal <= $2
ORDER BY ordinal
LIMIT 100

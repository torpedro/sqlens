-- Deterministic, fictional shop data for screenshots and manual exploration.
-- The Rust generator runs this file in a transaction in a new database.
PRAGMA user_version = 1;

CREATE TABLE customers (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    city TEXT NOT NULL,
    email TEXT NOT NULL UNIQUE,
    joined_on TEXT NOT NULL,
    profile TEXT NOT NULL CHECK (json_valid(profile)),
    notes TEXT
);

WITH RECURSIVE numbers(id) AS (
    VALUES (1) UNION ALL SELECT id + 1 FROM numbers WHERE id < 120
), people(slot, name, city) AS (
    VALUES
        (0, 'Ada Morgan', 'London'),
        (1, 'Ren Tanaka', 'Tokyo'),
        (2, 'Sofia Costa', 'Lisbon'),
        (3, 'Zoë Martin', 'Paris'),
        (4, 'Luca Rossi', 'Milan'),
        (5, 'Amara Okafor', 'Lagos'),
        (6, '林晓', 'Shanghai'),
        (7, 'Inês Silva', 'Porto'),
        (8, 'Noah Williams', 'Toronto'),
        (9, 'Maya Patel', 'Mumbai'),
        (10, 'León García', 'Madrid'),
        (11, 'Emma Jensen', 'Copenhagen')
)
INSERT INTO customers
SELECT id,
       CASE WHEN id <= 12 THEN name ELSE printf('Customer %03d', id) END,
       city,
       printf('customer%03d@example.test', id),
       date('2025-01-01', printf('+%d days', id * 2)),
       json_object(
           'plan', CASE WHEN id % 3 = 0 THEN 'team' ELSE 'personal' END,
           'active', json(CASE WHEN id % 7 = 0 THEN 'false' ELSE 'true' END),
           'preferences', json_object('newsletter', json('true'), 'currency', 'EUR'),
           'tags', json_array('online', CASE WHEN id % 4 = 0 THEN 'returning' ELSE 'new' END)
       ),
       CASE WHEN id = 1 THEN 'Prefers delivery after 17:00.' || char(10) || 'Leave with reception.'
            WHEN id = 4 THEN 'Gift message: [bold]Thank you![/bold]'
            WHEN id % 5 = 0 THEN 'Contact before dispatch.'
            ELSE NULL END
FROM numbers JOIN people ON slot = (id - 1) % 12
ORDER BY id;

CREATE TABLE products (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    category TEXT NOT NULL,
    price REAL NOT NULL CHECK (price >= 0),
    in_stock INTEGER NOT NULL,
    sample_bytes BLOB
);

INSERT INTO products VALUES
    (1, 'Linen notebook', 'Stationery', 18.50, 84, X'53514C656E7300FF'),
    (2, 'Ceramic mug', 'Home', 24.00, 32, NULL),
    (3, 'Canvas tote', 'Accessories', 16.00, 120, NULL),
    (4, 'Desk lamp', 'Home', 65.00, 18, X'000102030405FEFF'),
    (5, 'Fountain pen', 'Stationery', 42.50, 46, NULL),
    (6, 'Wool scarf', 'Accessories', 38.00, 27, NULL),
    (7, 'Travel flask', 'Outdoors', 29.00, 63, NULL),
    (8, 'Reading light', 'Home', 22.00, 0, NULL);

CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    customer_id INTEGER NOT NULL REFERENCES customers(id),
    product_id INTEGER NOT NULL REFERENCES products(id),
    ordered_on TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'shipped', 'delivered')),
    quantity INTEGER NOT NULL CHECK (quantity > 0),
    unit_price REAL NOT NULL,
    total REAL GENERATED ALWAYS AS (round(quantity * unit_price, 2)) STORED
);

WITH RECURSIVE numbers(id) AS (
    VALUES (1) UNION ALL SELECT id + 1 FROM numbers WHERE id < 360
)
INSERT INTO orders(id, customer_id, product_id, ordered_on, status, quantity, unit_price)
SELECT numbers.id,
       (numbers.id - 1) % 120 + 1,
       products.id,
       date('2026-01-01', printf('+%d days', (numbers.id - 1) / 3)),
       CASE numbers.id % 5 WHEN 0 THEN 'pending' WHEN 1 THEN 'shipped' ELSE 'delivered' END,
       (numbers.id - 1) % 3 + 1,
       products.price
FROM numbers JOIN products ON products.id = (numbers.id - 1) % 8 + 1
ORDER BY numbers.id;

CREATE INDEX orders_customer ON orders(customer_id);
CREATE INDEX orders_date ON orders(ordered_on);

CREATE VIEW recent_orders AS
SELECT orders.id, customers.name AS customer, products.name AS product,
       orders.ordered_on, orders.status, orders.quantity, orders.total
FROM orders
JOIN customers ON customers.id = orders.customer_id
JOIN products ON products.id = orders.product_id
WHERE ordered_on >= '2026-04-01';

CREATE VIEW customer_totals AS
SELECT customers.id, customers.name, customers.city,
       count(orders.id) AS order_count, round(sum(orders.total), 2) AS spent
FROM customers
LEFT JOIN orders ON orders.customer_id = customers.id
GROUP BY customers.id;

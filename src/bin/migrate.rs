use dotenvy::dotenv;
use sqlx::MySqlPool;

const UUID_V7_FUNCTION: &str = r#"
CREATE FUNCTION uuid_v7()
RETURNS CHAR(36)
NOT DETERMINISTIC
NO SQL
RETURN (
    SELECT LOWER(CONCAT(
        LEFT(timestamp_ms, 8), '-', RIGHT(timestamp_ms, 4), '-7',
        SUBSTRING(random_hex, 1, 3), '-',
        ELT((ORD(RANDOM_BYTES(1)) & 3) + 1, '8', '9', 'a', 'b'),
        SUBSTRING(random_hex, 4, 3), '-',
        SUBSTRING(random_hex, 7, 12)
    ))
    FROM (
        SELECT
            LPAD(
                HEX(FLOOR(UNIX_TIMESTAMP(CURRENT_TIMESTAMP(3)) * 1000)),
                12,
                '0'
            ) AS timestamp_ms,
            HEX(RANDOM_BYTES(16)) AS random_hex
    ) AS uuid_v7_source
)
"#;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();

    let database_url = std::env::var("DATABASE_URL")?;
    let pool = MySqlPool::connect(&database_url).await?;

    let function_exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.routines \
         WHERE routine_schema = DATABASE() \
           AND routine_name = 'uuid_v7' \
           AND routine_type = 'FUNCTION'",
    )
    .fetch_one(&pool)
    .await?;
    if function_exists == 0 {
        println!("Creating MySQL uuid_v7() compatibility function...");
        sqlx::raw_sql(UUID_V7_FUNCTION).execute(&pool).await?;
    } else {
        println!("MySQL uuid_v7() compatibility function already exists.");
    }

    println!("Applying SQLx migrations...");
    sqlx::migrate!().run(&pool).await?;
    println!("SQLx migrations completed successfully.");
    Ok(())
}

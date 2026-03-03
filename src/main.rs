use anyhow::{anyhow, Result};
use chrono::Local;
use tabled::builder::Builder;
use console::{style, Term};
use csv::ReaderBuilder;
use dialoguer::{Confirm, Input, Select};
use owo_colors::OwoColorize;
use serde_json::Value;
use sqlx::mysql::MySqlPoolOptions;
use sqlx::{Column, MySqlPool, Row, TypeInfo};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use tabled::settings::object::Columns;
use tabled::settings::{Alignment, Modify, Style};
use tabled::Table;
use tokio::io::{join, AsyncWriteExt};

const PAGE_SIZE: usize = 20;
const PREVIEW_LIMIT: usize = 5;

#[derive(Debug,Clone)] //过程宏
struct AppConfig {
    host:String,
    port:u16,
    password: String,
    database: String,
    read_only: bool,
    user: String,
}

//列信息
#[derive(Debug,Clone)]
struct ColumnInfo{
    name: String,
    data_type: String,
    is_nullable: bool,
    is_primary: bool,
}

struct TableInfo{
    name: String,
    row_count:i64,
    columns: Vec<ColumnInfo>,
    //类似C的vector<ColumnInfo>
}

//用户界面结构.可以被视作一个类的声明
struct RetroUI{
    term:Term,
    //类型是 console 库提供的 Term（终端控制对象）
}

//impl实现块，这里为RetroUI类型定义方法
impl RetroUI {
    fn new() -> Self{ //定义关联函数
        Self{
            term: Term::stdout(), //获取标准输出终端对象
        }
    }

    //&self表示借用，不转所有权
    fn Clear(&self){
        print!("\x1B[2J\x1B[1;1H");
        std::io::stdout().flush().unwrap();
    }

    fn header(&self,title: &str){ //切片借用
        let width:usize = 60;
        let padding = (width.saturating_sub(title.len())) / 2;
        println!("{}", "╔".repeat(width).cyan());
        println!("{}{}{}","".repeat(padding).on_cyan(),
                title.bold().white().on_cyan(),
                "".repeat(width.saturating_sub(padding + title.len())).on_cyan()
        );
        println!("{}","╚".repeat(width).cyan());
        println!();
    }

    fn breadcrumb(&self,path: &[&str]){
        let styled: Vec<String> = path.iter().enumerate().map(|(i,s)| {
            if i ==path.len() - 1{
                s.yellow().bold().to_string()
            } else {
                format!("{} {}" , s.dimmed(), ">".dimmed())
            }
        }).collect();
        println!("{}", styled.join(" "));
    }

    //执行成功
    fn success(&self,msg:&str) {
        println!("\n{} {}\n", "[✓]".green().bold(), msg.green());
    }

    //警告
    fn warning(&self, msg: &str) {
        println!("\n{} {}\n", "[!]".yellow().bold(), msg.yellow());
    }

    //报错
    fn error(&self, msg: &str) {
        println!("\n{} {}\n", "[✗]".red().bold(), msg.red());
    }

    //提示
    fn info(&self, msg: &str) {
        println!("{} {}", "[i]".dimmed(), msg.dimmed());
    }

    //等待下一步操作
    fn wait_for_key(&self) {
        println!("\n{}", "按 Enter 键继续...".dimmed());
        let _ = self.term.read_line();
    }

    fn print_table_simple(&self,headers: &[String], rows: &[Vec<String>]){
        if rows.is_empty(){
            self.info("(暂无数据)");
            return;
        }

        let mut data: Vec<Vec<String>> = Vec::with_capacity(rows.len() + 1);
        data.push(headers.iter().map(|h| h.cyan().to_string()).collect());
        data.extend(rows.iter().cloned());

        let mut table = Builder::from_iter(data).build();
        table.with(Style::psql());
        table.with(Modify::new(Columns::first()).with(Alignment::left()));

        println!("{}", table);
        println!("{}", format!("共{}行", rows.len()).dimmed());

    }

    fn show_impact_preview(&self,headers: &[String],rows:&[Vec<String>],total_count:usize){
        self.warning(&format!("此操作将影响 {} 条记录，预览前 {} 行：", total_count, rows.len()));
        self.print_table_simple(headers, rows);

        if total_count > rows.len(){
            println!("{}",format!("... 还有 {} 行未显示",total_count - rows.len()).dimmed());
        }
    }
}

//数据库服务项数据
struct DatabaseService {
    pool: MySqlPool,
    current_db: String,
}

impl DatabaseService {
    //数据库链接设置
    async fn connect(config: &AppConfig) -> Result<Self>{
        let url = format!(
            "mysql://{}:{}@{}:{}/{}",
            config.user, config.password, config.host, config.port, config.database
        );

        let pool = MySqlPoolOptions::new()
            .max_connections(3)
            .connect(&url)
            .await?;

        Ok(Self{
            pool,
            current_db: config.database.clone(),
        })
    }

    async fn get_tables(&self) -> Result<Vec<TableInfo>> {
        //SQL代码
        let sql = format!(
            "SELECT table_name, table_rows
             FROM information_schema.tables
             WHERE table_schema = '{}'
             ORDER BY table_name",
            self.current_db
        );

        let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;
        let mut tables = Vec::new();

        for row in rows {
            let name: String = row.try_get("table_name")?;
            let count:i64 = row.try_get("table_rows").unwrap_or(0);
            let columns = self.get_columns(&name).await?;

            tables.push(TableInfo{
            name,
            row_count:count,
            columns,
            });
        }
        Ok(tables)
    }

    async fn get_columns(&self,table: &str) -> Result<Vec<ColumnInfo>> {
        let sql = format!("DESCRIBE `{}`",table);
        let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;

        let mut columns = Vec::new();
        for row in rows {
            let field: String = row.try_get("field")?;
            let type_: String = row.try_get("Type")?;
            let null: String = row.try_get("Null")?;
            let key: String = row.try_get("Key")?;

            columns.push(ColumnInfo{
            name:field,
            data_type:type_,
            is_nullable: null == "YES",
            is_primary: key == "PRI",
            })
        }
        Ok(columns)
    }
}


fn main() {
}

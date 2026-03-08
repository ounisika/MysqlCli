use anyhow::{anyhow, Result};
use chrono::Local;
use tabled::builder::Builder;
use console::{style, Term};
use csv::ReaderBuilder;
use dialoguer::{Confirm, Input, Select};
use owo_colors::OwoColorize;
use serde_json::{to_string, Value};
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

    async fn query_data(&self,table: &str, limit:usize,offset:usize,where_clause: Option<&str>)
        -> Result<(Vec<String>, Vec<Vec<String>>)>{
        let sql = if let Some(w) = where_clause{
            format!("SELECT * FROM `{}` WHERE {} LIMIT {} OFFSET {}",table,w,limit,offset)
        } else {
            format!("SELECT * FROM `{}` LIMIT {} OFFSET {}",table,limit,offset)
        };
        let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;

        if rows.is_empty(){
            return Ok((vec![], vec![]));
        }

        let headers:Vec<String> = rows[0]
            .columns()
            .iter()
            .map(|c| c.name().to_string())
            .collect();

        let data:Vec<Vec<String>> =rows
            .iter()
            .map(|row|{
                headers.iter()
                    .enumerate()
                    .map(|(i,_)| self.value_to_string(row,i))
                    .collect()
            }).collect();

        Ok((headers,data))
    }

    fn value_to_string(&self,row:&sqlx::mysql::MySqlRow,index:usize) -> String{
        if let Ok(val) = row.try_get::<Option<String>,_>(index){
            val.unwrap_or_else(||"NULL".to_string())
        }else if let Ok(val) = row.try_get::<Option<f64>,_>(index) {
            val.map(|v| v.to_string()).unwrap_or_else(|| "NULL".to_string())
        }else if let Ok(val) = row.try_get::<Option<i64>,_>(index){
            val.map(|v| format!("{:.2}",v)).unwrap_or_else(|| "NULL".to_string())
        }else {
            "<?>".to_string()
        }
    }

    //统计行数
    async fn count_rows(&self,table: &str,where_clause: &str) -> Result<usize> {
        let sql = format!("SELECT COUNT(*) FROM `{}` WHERE {}", table, where_clause);
        let row = sqlx::query(&sql).fetch_one(&self.pool).await?;
        let count: i64 = row.try_get("c")?;
        Ok(count as usize)
    }

    //插入单行
    async fn insert_row(&self, table: &str,data: &HashMap<String, String>) ->Result<u64> {
        if data.is_empty(){
            return Err(anyhow!("没有数据要插入！"));
        }

        let columns: Vec<_> = data.keys().map(|k|format!("`{}`",k)).collect();
        let placeholders:Vec<_> = data.keys().map(|_| "?").collect();

        //翻译成SQL指令
        let sql = format!(
            "INSERT INTO `{}` ({}) VALUES ({})",
            table,
            columns.join(", "),
            placeholders.join(", ")
        );

        let mut query = sqlx::query(&sql);
        for key in data.keys(){
            query = query.bind(data.get(key).unwrap());
        }

        let result = query.execute(&self.pool).await?;
        Ok(result.rows_affected())
    }

    //更新指定单元格
    async fn update_cell(&self,table: &str,primary_key: &str,pk_value: &str,column: &str,new_value: &str) ->Result<u64>{
        let sql = format!(
            "UPDATE `{}` SET `{}` = ? WHERE `{}` = ? LIMIT 1",
            table, column, primary_key
        );

        let result = sqlx::query(&sql)
            .bind(new_value)
            .bind(pk_value)
            .execute(&self.pool).await?;

        Ok(result.rows_affected())
    }

    //删除单行
    async fn delete_row(&self, table: &str,where_clause: &str) -> Result<u64> {
        let sql = format!("DELETE FROM `{}` WHERE {}", table, where_clause);
        let result = sqlx::query(&sql).execute(&self.pool).await?;
        Ok(result.rows_affected())
    }
}

//程序状态
enum AppState {
    Mainmenu,
    TableMenu(String),
    ViewData(String, usize, Option<String>),
    AddRecord(String),
    QuickEdit(String, String),
    BatchInsert(String),
    BatchDelete(String),
    BuildWhere(String),
}

//初始化App数据
struct App{
    ui: RetroUI,
    config: AppConfig,
    db: DatabaseService,
    state: AppState,
    pending_where_clause: Option<String>,
}

//主App功能实现
impl App {
    async fn new(config:AppConfig) -> Result<Self>{
        //链接数据库
        let db = DatabaseService::connect(&config).await?;
        Ok(Self{
            ui:RetroUI::new(),
            db,
            config,
            state:AppState::Mainmenu,
            pending_where_clause:None,
        })
    }

    //CLI状态机主循环
    async fn run(&mut self) -> Result<()>{
        loop{
            match &self.state{
                AppState::Mainmenu => self.main_menu().await?,
                AppState::TableMenu(table) => {
                    let table = table.clone();
                    self.table_menu(&table).await?;
                },
                AppState::ViewData(table,page,filter){
                    let table = table.clone();
                    let page = *page;
                    let filter = filter.clone();
                    self.view_data(&table, page, filter.as_deref()).await?
                },
            }
        }
    }

    ////////////////////////以下区域为页面实现//////////////////////////////////////////
    //主页面实现
    async fn main_menu(&mut self) ->Result<()>{
        //清屏
        self.ui.Clear();
        //设置标题
        self.ui.header("Mysql-CLI");

        let tables = self.db.get_tables().await?;

        println!("{}\n", "请选择要操作的表：".bold());

        let items:Vec<String> = tables
            .iter()
            .map(|t| format!("{:<20} ({} 条记录)", t.name, t.row_count))
            .collect();

        let selection = Select::new()
            .items(&items)
            .default(0)
            .interact()?;

        let selected_table = tables[selection].name.clone();
        self.state = AppState::TableMenu(selected_table);
        Ok(())
    }
    //菜单页面
    async fn table_menu(&mut self,table: &str) ->Result<()>{
        self.ui.Clear();
        self.ui.breadcrumb(&["主菜单",table]);

        //菜单选项
        let options = vec![
            "查看数据",
            "插入单条数据",
            "批量导入",
            "修改单条记录",
            "批量删除记录",
            "查看表结构",
            "返回上级",
        ];
        let selection = Select::new()
            .with_prompt("请选择操作")
            .items(&options)
            .default(0)
            .interact()?;

        match selection{
            0 => self.state = AppState::ViewData(table.to_string(), 0, None),
            1 =>{
                if self.check_readonly() {return Ok(());}
                self.state = AppState::AddRecord(table.to_string());
            },
            2 => {
                if self.check_readonly() { return Ok(()); }
                self.state = AppState::BatchInsert(table.to_string());
            },
            3 => {
                if self.check_readonly() { return Ok(()); }
                self.state = AppState::ViewData(table.to_string(), 0, None);
            },
            4 => {
                if self.check_readonly() { return Ok(()); }
                self.state = AppState::BatchDelete(table.to_string());
            },
            5 => self.show_schema(table).await?,
            6 => self.state = AppState::Mainmenu,
            _ => {}
        }
        Ok(())
    }

    fn check_readonly(&self) -> bool {
        if self.config.read_only {
            self.ui.error("当前为只读模式，禁止修改数据");
            self.ui.wait_for_key();
            true
        } else {
            false
        }
    }

    async fn view_data(&mut self, table: &str, page: usize, filter: Option<&str>) -> Result<()> {
        self.ui.Clear();

        let breadcrumb
    }
}

fn main() {
}

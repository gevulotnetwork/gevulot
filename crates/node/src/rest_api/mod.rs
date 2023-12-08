use std::sync::Arc;

use actix_web::{get, post, web, HttpResponse, Responder, Result};
use tokio::sync::RwLock;

use crate::{
    asset_manager::AssetManager,
    mempool::Mempool,
    storage::{Database, File as FileStorage},
    types::{Program, Task},
};

pub struct AppState {
    pub asset_manager: Arc<AssetManager>,
    pub database: Arc<Database>,
    pub file_storage: Arc<FileStorage>,
    pub mempool: Arc<RwLock<Mempool<Task>>>,
}

#[get("/")]
async fn index() -> String {
    "Gevulot node".to_string()
}

#[get("/tasks")]
async fn tasks(state: web::Data<AppState>) -> Result<impl Responder> {
    if let Ok(tasks) = state.database.get_tasks().await {
        return Ok(web::Json(tasks));
    }

    Ok(web::Json(vec![]))
}

#[post("/tasks")]
async fn add_task(state: web::Data<AppState>, task: web::Json<Task>) -> HttpResponse {
    let task = task.into_inner();

    tracing::info!(
        "received task: {}, program_id: {}",
        task.id,
        task.program_id.to_string()
    );

    for file in &task.files {
        // Task ID must be copied to File.
        let mut file = file.clone();
        file.task_id = task.id;

        let res = state.file_storage.download(&file).await;
        if res.is_err() {
            return HttpResponse::InternalServerError()
                .body(format!("failed to download file: {}", res.err().unwrap()));
        }
    }

    if state.mempool.write().await.add(task).await.is_ok() {
        return HttpResponse::Accepted().finish();
    }

    HttpResponse::InternalServerError().body("failed to add task")
}

#[get("/programs")]
async fn programs(state: web::Data<AppState>) -> Result<impl Responder> {
    if let Ok(programs) = state.database.get_programs().await {
        return Ok(web::Json(programs));
    }

    Ok(web::Json(vec![]))
}

#[post("/programs")]
async fn deploy_program(state: web::Data<AppState>, program: web::Json<Program>) -> HttpResponse {
    /*
    let program = program.into_inner();
    if state.database.add_program(&program).await.is_ok() {
        return HttpResponse::Accepted().finish();
    }
    */

    HttpResponse::InternalServerError().body("failed to deploy program")
}

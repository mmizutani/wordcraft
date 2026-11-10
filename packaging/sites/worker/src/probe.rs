use worker::{Bucket, D1Database, Env, Range, Request, Response, Result, wasm_bindgen::JsValue};

const PAYLOAD: &[u8] = b"wordcraft-sites-probe-v1";

pub(crate) async fn respond(request: &Request, env: &Env) -> Result<Response> {
    let mut response = match dispatch(request, env).await {
        Ok(response) => response,
        Err(_) => Response::error("Probe failed", 500)?,
    };
    response.headers_mut().set("Cache-Control", "no-store")?;
    Ok(response)
}

async fn dispatch(request: &Request, env: &Env) -> Result<Response> {
    // The owner-only Sites dispatcher replaces this header before invoking this Worker.
    let identity = request.headers().get("oai-authenticated-user-id")?;
    let url = request.url()?;
    let id = match super::probe_id(identity.as_deref(), url.query()) {
        Ok(id) => id,
        Err(401) => return Response::error("Authentication required", 401),
        Err(_) => return Response::error("Invalid probe ID", 400),
    };
    let method = request.inner().method();
    if !matches!(method.as_str(), "PUT" | "GET" | "DELETE") {
        let mut response = Response::error("Method not allowed", 405)?;
        response.headers_mut().set("Allow", "PUT, GET, DELETE")?;
        return Ok(response);
    }

    let owner = identity.as_deref().ok_or("Missing probe owner")?;
    let db = env.d1("DB")?;
    let bucket = env.bucket("BUCKET")?;
    if method == "PUT" {
        return write(&db, &bucket, owner, id).await;
    }

    let key = db
        .prepare("SELECT object_key FROM wordcraft_probe WHERE owner = ?1 AND id = ?2")
        .bind(&[JsValue::from_str(owner), JsValue::from_str(id)])?
        .first::<String>(Some("object_key"))
        .await?;
    let Some(key) = key else {
        return Response::error("Probe not found", 404);
    };

    if method == "DELETE" {
        remove_object(&bucket, &key).await?;
        remove_record(&db, owner, id, &key).await?;
        return Ok(Response::empty()?.with_status(204));
    }
    read(&bucket, &key).await
}

async fn write(db: &D1Database, bucket: &Bucket, owner: &str, id: &str) -> Result<Response> {
    // A fresh key lets a losing writer clean up without deleting the winning writer's bytes.
    let key = db
        .prepare("SELECT 'wordcraft-probe/' || lower(hex(randomblob(16))) AS object_key")
        .first::<String>(Some("object_key"))
        .await?
        .ok_or("No probe object key")?;
    let insert = db
        .prepare(
            "INSERT INTO wordcraft_probe (owner, id, object_key) VALUES (?1, ?2, ?3) \
             ON CONFLICT (owner, id) DO NOTHING RETURNING object_key",
        )
        .bind(&[JsValue::from_str(owner), JsValue::from_str(id), JsValue::from_str(&key)])?;

    match bucket.put(&key, PAYLOAD.to_vec()).execute().await {
        Ok(Some(_)) => {}
        _ => {
            remove_object(bucket, &key).await?;
            return Response::error("Probe write failed", 500);
        }
    }

    match insert.first::<String>(Some("object_key")).await {
        Ok(Some(_)) => Ok(Response::empty()?.with_status(201)),
        Ok(None) => {
            remove_object(bucket, &key).await?;
            Response::error("Probe already exists", 409)
        }
        Err(_) => {
            // An insert can fail after committing. Remove its exact row before its bytes.
            remove_record(db, owner, id, &key).await?;
            remove_object(bucket, &key).await?;
            Response::error("Probe write failed", 500)
        }
    }
}

async fn read(bucket: &Bucket, key: &str) -> Result<Response> {
    let object = bucket.get(key).range(Range::Prefix { length: PAYLOAD.len() as u64 + 1 }).execute().await?.ok_or("Missing probe object")?;
    let body = object.body().ok_or("Missing probe body")?;
    let bytes = body.bytes().await?;
    if bytes != PAYLOAD {
        return Response::error("Probe bytes differ", 500);
    }
    Response::from_bytes(bytes)
}

async fn remove_object(bucket: &Bucket, key: &str) -> Result<()> {
    bucket.delete(key).await?;
    if bucket.head(key).await?.is_some() {
        return Err("Probe object still exists".into());
    }
    Ok(())
}

async fn remove_record(db: &D1Database, owner: &str, id: &str, key: &str) -> Result<()> {
    db.prepare(
        "DELETE FROM wordcraft_probe WHERE owner = ?1 AND id = ?2 AND object_key = ?3
         RETURNING object_key",
    )
    .bind(&[JsValue::from_str(owner), JsValue::from_str(id), JsValue::from_str(key)])?
    .first::<String>(Some("object_key"))
    .await?;
    Ok(())
}

use super::*;

/// `GET /api/edit/games/{id}/HashSalt` — the per-game team-hash salt
/// (`Game.TeamHashSalt` = `sha256("RSCTF@{PrivateKey}@PK")`). Contract: raw
/// `string`.
pub async fn get_hash_salt(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<String>> {
    manager_or_admin(&st, &user, id).await?;
    let g = load_game(&st, id).await?;
    let salt = sha256_str(&format!("RSCTF@{}@PK", g.private_key));
    Ok(RequestResponse::ok(salt))
}

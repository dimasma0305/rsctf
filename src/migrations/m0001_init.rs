use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::Schema;

use crate::models::data::{
    ad_attack, ad_check_result, ad_flag, ad_round, ad_team_service, api_token, attachment,
    challenge_review, cheat_info, config, container, division, division_challenge_config,
    exercise_challenge, exercise_instance, first_solve, flag_context, game, game_challenge,
    game_event, game_instance, game_notice, koth_target, koth_token, local_file, participation,
    post, submission, team, user, user_participation,
};

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Create the table for `$entity`, tolerating re-runs with `if_not_exists`.
macro_rules! create {
    ($manager:expr, $schema:expr, $entity:expr) => {{
        let mut stmt = $schema.create_table_from_entity($entity);
        stmt.if_not_exists();
        $manager.create_table(stmt).await?;
    }};
}

macro_rules! drop_table {
    ($manager:expr, $entity:expr) => {{
        $manager
            .drop_table(Table::drop().table($entity).if_exists().to_owned())
            .await?;
    }};
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let schema = Schema::new(backend);

        create!(manager, schema, user::Entity);
        create!(manager, schema, team::Entity);
        create!(manager, schema, user_participation::Entity);
        create!(manager, schema, game::Entity);
        create!(manager, schema, division::Entity);
        create!(manager, schema, division_challenge_config::Entity);
        create!(manager, schema, game_notice::Entity);
        create!(manager, schema, game_event::Entity);
        create!(manager, schema, game_challenge::Entity);
        create!(manager, schema, flag_context::Entity);
        create!(manager, schema, attachment::Entity);
        create!(manager, schema, local_file::Entity);
        create!(manager, schema, challenge_review::Entity);
        create!(manager, schema, participation::Entity);
        create!(manager, schema, submission::Entity);
        create!(manager, schema, game_instance::Entity);
        create!(manager, schema, container::Entity);
        create!(manager, schema, first_solve::Entity);
        create!(manager, schema, cheat_info::Entity);
        create!(manager, schema, post::Entity);
        create!(manager, schema, config::Entity);
        create!(manager, schema, api_token::Entity);
        create!(manager, schema, exercise_challenge::Entity);
        create!(manager, schema, exercise_instance::Entity);
        create!(manager, schema, ad_round::Entity);
        create!(manager, schema, ad_team_service::Entity);
        create!(manager, schema, ad_flag::Entity);
        create!(manager, schema, ad_attack::Entity);
        create!(manager, schema, ad_check_result::Entity);
        create!(manager, schema, koth_target::Entity);
        create!(manager, schema, koth_token::Entity);

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        drop_table!(manager, koth_token::Entity);
        drop_table!(manager, koth_target::Entity);
        drop_table!(manager, ad_check_result::Entity);
        drop_table!(manager, ad_attack::Entity);
        drop_table!(manager, ad_flag::Entity);
        drop_table!(manager, ad_team_service::Entity);
        drop_table!(manager, ad_round::Entity);
        drop_table!(manager, exercise_instance::Entity);
        drop_table!(manager, exercise_challenge::Entity);
        drop_table!(manager, api_token::Entity);
        drop_table!(manager, config::Entity);
        drop_table!(manager, post::Entity);
        drop_table!(manager, cheat_info::Entity);
        drop_table!(manager, first_solve::Entity);
        drop_table!(manager, container::Entity);
        drop_table!(manager, game_instance::Entity);
        drop_table!(manager, submission::Entity);
        drop_table!(manager, participation::Entity);
        drop_table!(manager, challenge_review::Entity);
        drop_table!(manager, local_file::Entity);
        drop_table!(manager, attachment::Entity);
        drop_table!(manager, flag_context::Entity);
        drop_table!(manager, game_challenge::Entity);
        drop_table!(manager, game_event::Entity);
        drop_table!(manager, game_notice::Entity);
        drop_table!(manager, division_challenge_config::Entity);
        drop_table!(manager, division::Entity);
        drop_table!(manager, game::Entity);
        drop_table!(manager, user_participation::Entity);
        drop_table!(manager, team::Entity);
        drop_table!(manager, user::Entity);
        Ok(())
    }
}

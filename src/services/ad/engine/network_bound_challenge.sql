(challenge.ad_self_hosted OR EXISTS (
    SELECT 1 FROM "AdTeamServices" hosted
     WHERE hosted.game_id = challenge.game_id AND hosted.challenge_id = challenge.id
       AND hosted.container_id LIKE 'rsctf-worker:%'
))

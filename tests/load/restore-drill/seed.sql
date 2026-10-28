-- Values substituted here are fixture-generated UUIDs, SHA-256 hex and integers.
BEGIN;
SELECT set_config('rsctf.identity_neutral_insert', '1', true);
INSERT INTO "AspNetUsers"
 (id,user_name,normalized_user_name,email,normalized_email,email_confirmed,
 security_stamp,phone_number_confirmed,two_factor_enabled,lockout_enabled,
 access_failed_count,role,ip,last_signed_in_utc,last_visited_utc,register_time_utc,
 bio,real_name,std_number,exercise_visible)
VALUES (':USER:','restore-admin','RESTORE-ADMIN','restore@example.test',
 'RESTORE@EXAMPLE.TEST',true,'restore-stamp',false,false,false,0,3,'',
 now(),now(),now(),'','','',true);
INSERT INTO "Files" (id,hash,upload_time_utc,file_size,name,reference_count) VALUES
 (1,':POSTER:',now(),:POSTER_SIZE:,'poster',1),
 (2,':WRITEUP:',now(),:WRITEUP_SIZE:,'writeup.pdf',1);
INSERT INTO "Attachments" (id,"Type",local_file_id) VALUES (1,1,2);
INSERT INTO "Games"
 (id,title,public_key,private_key,hidden,practice_mode,poster_hash,summary,content,
 accept_without_review,allow_user_submissions,writeup_required,team_member_count_limit,
 container_count_limit,start_time_utc,end_time_utc,writeup_deadline,writeup_note,
 blood_bonus_value,ad_allow_snapshot_download,ad_scoring_paused)
VALUES (1,'Restore fixture','','',false,false,':POSTER:','','',true,false,true,
 5,2,now()-interval '2 days',now()-interval '1 day',now()+interval '1 day','',
 0,false,false);
INSERT INTO "Teams" (id,name,locked,invite_token,captain_id)
VALUES (1,'Restored team',false,'restore-team',':USER:');
INSERT INTO "Participations"
 (id,status,token,writeup_id,game_id,team_id,suspicion_score)
VALUES (1,1,'restore-participation',1,1,1,0);
INSERT INTO "UserParticipations" (user_id,game_id,team_id,participation_id)
VALUES (':USER:',1,1,1);
INSERT INTO "GameChallenges"
 (id,game_id,title,content,category,"Type",is_enabled,submission_limit,accepted_count,
 submission_count,review_status,build_status,enable_traffic_capture,enable_shared_container,
 original_score,min_score_rate,difficulty,score_curve,ad_allow_egress,ad_allow_self_reset,
 ad_ssh_requires_flag,ad_self_hosted)
VALUES (1,1,'Restored solve','',0,0,true,0,1,1,0,4,false,false,100,0.25,5,0,
 false,false,false,false);
INSERT INTO "GameInstances" (challenge_id,participation_id,is_loaded,last_container_operation)
VALUES (1,1,false,now());
INSERT INTO "Submissions"
 (answer,status,submit_time_utc,user_id,team_id,participation_id,game_id,challenge_id)
VALUES ('restore-fixture-answer',1,now()-interval '36 hours',':USER:',1,1,1,1);
INSERT INTO "FirstSolves" (participation_id,challenge_id,submission_id)
SELECT 1,1,id FROM "Submissions" WHERE challenge_id=1;
COMMIT;

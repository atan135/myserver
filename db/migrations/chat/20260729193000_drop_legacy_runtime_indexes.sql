-- Logical owner: chat-server
-- Compatibility phase: contract
-- Irreversible risk: none
-- Transaction: required
-- Lock timeout: 5s
-- Statement timeout: 60s
-- Backup point: not-required
-- Recovery command: SQLx rolls back the transaction; correct the migration and rerun db up.

-- Older chat-server images created these duplicate indexes during startup. The
-- initial migration owns the canonical idx_sender/idx_target/etc. indexes.
-- Drop only the legacy duplicates; IF EXISTS keeps fresh installations safe.
DROP INDEX IF EXISTS public.idx_chat_group_members_player;
DROP INDEX IF EXISTS public.idx_chat_groups_owner;
DROP INDEX IF EXISTS public.idx_chat_messages_created;
DROP INDEX IF EXISTS public.idx_chat_messages_group;
DROP INDEX IF EXISTS public.idx_chat_messages_sender;
DROP INDEX IF EXISTS public.idx_chat_messages_target;

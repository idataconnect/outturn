-- Claude on Amazon Bedrock, through its Invoke API: a protocol a route may
-- name. See src/gateway/llm/provider/bedrock.rs.
alter table traffic_routes drop constraint traffic_routes_provider_check;
alter table traffic_routes add constraint traffic_routes_provider_check
    check (provider in ('openai', 'anthropic', 'gemini', 'bedrock'));

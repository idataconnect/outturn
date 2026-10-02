-- An egress rule may exchange OAuth 2 client credentials for its token rather
-- than attach a static one. See docs/client-credentials.md.
--
-- The id and secret are named the way credential_env is, and the token they
-- buy is held in the gateway's memory and never here: a token is a working
-- credential for as long as it lasts, and nothing that reads this table may be
-- able to leak one by reading it.
alter table egress_rules
    -- The token endpoint, path included: multi-tenant providers put the tenant
    -- in the path, so the host alone would not say whose it is.
    add column token_url         text,
    -- Space-separated, sent exactly as written.
    add column scope             text,
    add column client_id_env     text,
    add column client_secret_env text,
    -- Where the id and secret travel in the exchange.
    add column client_auth       text check (client_auth in ('basic', 'post')),
    -- A static credential is a header and a variable together; an exchange is
    -- its URL, both variables and how they travel, together. Never both, since
    -- a rule attaching two credentials says nothing about which the API reads.
    add check ((header is null) = (credential_env is null)),
    add check (
        (token_url is null and scope is null and client_id_env is null
            and client_secret_env is null and client_auth is null)
        or (token_url is not null and client_id_env is not null
            and client_secret_env is not null and client_auth is not null
            and header is null)
    );

-- What a skill version's files declare needs approving.
--
-- Derived when a version is published rather than when a turn runs. The
-- declaration lives in the frontmatter of an operation's file
-- (docs/approvals.md), and the content of those files is in the object store by
-- hash -- so computing this per turn would mean reading every bound skill's every
-- file before the first token, on the path a person is waiting on. A version is
-- immutable, so what its files declare cannot change after this is written.
--
-- One row per gate rather than a JSON column, because the API has to sort and
-- hash them deterministically to commit to them, and a query that returns rows
-- in a defined order is easier to hold to that than a blob somebody appends to.
-- No `workspace_id`, which every other table here carries in its key. The
-- exemption is deliberate and worth stating, because AGENTS.md makes the rule
-- load-bearing: a version belongs to one skill and a skill to one workspace, and
-- an operator's skill belongs to the platform workspace while the turns reading it
-- belong to somebody else's. A `workspace_id` here would have to be the skill's,
-- which is not the turn's, so a query filtering on the turn's would find nothing
-- for exactly the skills the operator ships. The tenancy check is the version id,
-- which a turn only holds for skills it resolved.
create table skill_version_gates (
    version_id     uuid not null references skill_versions (id) on delete cascade,
    -- Which file said so, for an operator asking why a request is gated.
    path           text not null,
    -- The act, in a word: the `requires` of the declaration. Compared rather
    -- than read -- two operations declaring the same act are the same act for
    -- one grant.
    requires       text not null,
    -- The request shape, as the gateway will match it: a host from the skill's
    -- declared hosts, a method, and a path that may end in `*`.
    host           text not null,
    method         text not null,
    path_pattern   text not null,
    -- The body field naming the unit a wider grant may span. Null when the
    -- declaration offered no `covers`.
    identified_by  text,
    -- `requires` is in the key because one file may declare more than one act
    -- about the same operation -- charging and refunding a booking are documented
    -- together often enough. The sibling tables key on `(version_id, <one
    -- column>)` because a version has one host per host and one file per path;
    -- here a path is not unique on its own.
    --
    -- And `host`, because a skill's declaration is written once and applies to
    -- every host it names: the write loops over them inserting a row each. Left
    -- out, the second host collided with the first and was dropped by `on
    -- conflict do nothing` -- so adding a host to a gated skill quietly ungated
    -- it, and a request there went out with nobody asked. A gate that is not
    -- written is not a gate.
    primary key (version_id, path, requires, host)
);

-- No index of its own. The read is per version, and the primary key above already
-- leads with `version_id`, so it is served -- the sibling tables index their
-- *other* column because their keys lead with the version too, and the reverse
-- lookup is the one that needs help. Nothing here looks a gate up by host.

-- What a person approving one of these is actually approving ------------------

-- The request-body fields a grant is keyed on, in the order the skill declared
-- them.
--
-- Without them a `call` grant is keyed on method, host and path alone, which
-- reads as sufficient -- a retry of one call is the same shape by construction
-- -- and is not: two *different* charges are the same shape too, so approving
-- £40 for one booking let £4,000 for another straight through. See
-- docs/approvals.md.
--
-- Ordered, so `binds: [a, b]` and `binds: [b, a]` are different declarations and
-- reordering one invalidates the grants taken out under it. That is the safe
-- direction: a grant whose meaning quietly changed is worse than one that has to
-- be asked for again.
--
-- No `host` in the key, unlike the gate above. What a request binds does not
-- vary by where it is sent, so these are written once per declaration and the
-- repeats past the first host are no-ops.
create table skill_version_gate_binds (
    version_id  uuid    not null,
    path        text    not null,
    requires    text    not null,
    -- Position in the declaration, which is part of the digest.
    position    int     not null,
    field       text    not null,

    primary key (version_id, path, requires, position),
    foreign key (version_id) references skill_versions (id) on delete cascade
);

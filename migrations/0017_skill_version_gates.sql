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
    primary key (version_id, path, requires)
);

-- No index of its own. The read is per version, and the primary key above already
-- leads with `version_id`, so it is served -- the sibling tables index their
-- *other* column because their keys lead with the version too, and the reverse
-- lookup is the one that needs help. Nothing here looks a gate up by host.

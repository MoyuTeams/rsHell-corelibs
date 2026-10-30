CREATE TABLE configuration_revision(
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    revision INTEGER NOT NULL CHECK(typeof(revision) = 'integer' AND revision >= 0)
);

INSERT INTO configuration_revision(singleton, revision) VALUES(1, 0);

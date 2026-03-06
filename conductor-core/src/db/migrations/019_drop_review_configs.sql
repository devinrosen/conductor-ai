-- Drop the review_configs table; reviewer roles now live in
-- .conductor/reviewers/*.md files in each repo.
DROP TABLE IF EXISTS review_configs;

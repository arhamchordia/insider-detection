-- wallet_scores: one row per wallet, updated on each scoring run.
-- flagged is sticky — once TRUE it is never reset to FALSE.
CREATE TABLE IF NOT EXISTS wallet_scores (
    address             TEXT PRIMARY KEY,
    score               NUMERIC        NOT NULL,
    entry_timing_score  NUMERIC        NOT NULL,
    concentration_score NUMERIC        NOT NULL,
    size_score          NUMERIC        NOT NULL,
    wallet_age_score    NUMERIC        NOT NULL DEFAULT 0,
    win_rate_score      NUMERIC        NOT NULL,
    total_volume_usdc   NUMERIC        NOT NULL DEFAULT 0,
    markets_traded      INTEGER        NOT NULL DEFAULT 0,
    flagged             BOOLEAN        NOT NULL DEFAULT FALSE,
    first_activity_ts   TIMESTAMPTZ,
    scored_at           TIMESTAMPTZ    NOT NULL DEFAULT NOW()
);

-- known_insiders: calibration set of wallets with confirmed insider activity.
-- Seeded once; used to measure detection rate in the dashboard.
CREATE TABLE IF NOT EXISTS known_insiders (
    address TEXT PRIMARY KEY,
    label   TEXT NOT NULL,
    market  TEXT NOT NULL DEFAULT '',
    source  TEXT
);

INSERT INTO known_insiders (address, label, market, source) VALUES
  ('0xee50a31c3f5a7c77824b12a941a54388a2827ed6', 'Google d4vd',             'Google d4vd (music)',        'https://x.com/drizzl3r/status/1996434092749914596'),
  ('0x6baf05d193692bb208d616709e27442c910a94c5', 'Maduro SBet365',           'Maduro out of office',       'https://x.com/thejayden/status/2010844183301374290'),
  ('0x0afc7ce56285bde1fbe3a75efaffdfc86d6530b2', 'Israel Iran ricosuave',    'Israel-Iran ceasefire',      'https://x.com/AdameMedia/status/2009011970780037534'),
  ('0x7f1329ade2ec162c6f8791dad99125e0dc49801c', 'Trump pardon CZ',          'Trump CZ pardon market',     'https://x.com/Polysights/status/1977716009797570865'),
  ('0x31a56e9e690c621ed21de08cb559e9524cdb8ed9', 'Maduro unnamed',           'Maduro out of office',       'https://x.com/Andrey_10gwei/status/2007904168791454011'),
  ('0x976685b6e867a0400085b1273309e84cd0fc627c', 'Micro strategy fromagi',   'MicroStrategy stock market', 'https://x.com/Polysights/status/1997753083934204049'),
  ('0x55ea982cebff271722419595e0659ef297b48d7c', 'draftkings flaccidwillie', 'DraftKings listing market',  'https://x.com/Polysights/status/1999361742405611964')
ON CONFLICT DO NOTHING;

-- scorer_state: key-value store for enumeration cursor and run metadata.
-- Allows the scorer to resume pagination on restart.
CREATE TABLE IF NOT EXISTS scorer_state (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

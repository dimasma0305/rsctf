// Player load: discover the game's players/hills/challenges, mint player JWTs, and run
// the A&D + KotH player poll+submit k6 scenario against them.  `npm run player`
import { discover, runK6, TARGET, GAME } from './lib.mjs';

const d = discover();
console.log(`player load → ${TARGET} game=${GAME} | players=${d.tokens.length} hills=[${d.kothHills}] adChals=[${d.adChals}]`);
if (!d.tokens.length) {
  console.error('no accepted-participation players for this game — nothing to authenticate as');
  process.exit(1);
}
process.exit(
  runK6('player.js', {
    TARGET,
    GAME,
    TOKENS: d.tokens.join(','),
    KOTH_HILLS: d.kothHills,
    AD_CHALS: d.adChals,
    VUS: process.env.VUS || 250,
    RATE: process.env.RATE || '',
    DURATION: process.env.DURATION || '60s',
    THINK_MIN_SECONDS: process.env.THINK_MIN_SECONDS || '',
    THINK_MAX_SECONDS: process.env.THINK_MAX_SECONDS || '',
  })
);

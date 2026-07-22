import { mintJwt } from './lib.mjs';
console.log('ADMIN=' + mintJwt('019f876b-05db-7130-8132-6113c4665f85','0951b941-3314-486c-b2ea-395005156173',3));
console.log('USER=' + mintJwt('019f876b-3bb3-7ad3-a593-fba85a400997','c5882542-bb43-4539-b8f2-5dffe57f53f3',1));
console.log('MONITOR=' + mintJwt('019f876b-061a-7d92-955e-6fc08a63361a','649f67e4-4c31-4a0c-992c-6d712592eb9e',2));

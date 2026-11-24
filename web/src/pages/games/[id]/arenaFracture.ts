// Hand-drawn pane with one impact point. Shared junctions keep the splinters
// connected at every aspect ratio. Only presentation data is interpolated.
const cracks = [
  'M566 264 548 248 526 219 503 204 452 144 408 124 347 50 262 -12',
  'M568 260 561 233 565 217 550 163 553 127 529 72 542 -15',
  'M573 262 582 240 583 219 604 193 612 158 641 107 646 74 697 -12',
  'M577 265 604 249 621 235 647 230 689 193 722 187 782 145 836 136 937 59 1020 38',
  'M578 268 605 266 632 272 657 265 708 273 753 261 805 267 877 249 932 257 1010 237',
  'M577 272 603 285 621 286 647 310 686 324 732 361 776 369 853 425 901 435 1015 527',
  'M574 275 582 301 599 324 602 352 633 402 631 433 651 468 661 529 706 612 709 667',
  'M570 276 562 306 544 324 538 355 509 391 491 451 448 494 412 587 357 663',
  'M565 273 538 290 519 292 484 317 445 323 396 354 354 358 286 395 245 397 134 452 -12 495',
  'M563 268 532 267 512 256 481 259 438 243 396 247 342 228 279 231 208 212 123 212 -10 180',
]

const branches = 'M526 219 521 239 512 256 519 274 519 292 M550 163 530 185 503 204 M565 217 583 219 M604 193 621 205 621 235 632 249 632 272 621 286 616 307 599 324 569 317 544 324 532 308 519 292 M647 230 663 249 657 265 M708 273 699 295 686 324 M633 402 592 387 538 355 513 346 484 317 M445 323 451 298 438 243 M396 247 375 213 339 196 M452 144 453 107 439 84 M641 107 674 94 698 57 M782 145 790 116 818 86 M877 249 920 217 955 211 M776 369 794 405 848 453 M631 433 600 468 585 508 M396 354 385 380 359 404'

export const fractureGlass = `<svg class="fracture-glass" viewBox="0 0 1000 650" preserveAspectRatio="xMidYMid slice" aria-hidden="true" focusable="false">
  <defs>
    <linearGradient id="pane-glint" x1="0" y1="1" x2="1" y2="0" gradientUnits="objectBoundingBox">
      <stop stop-color="#b7f4ff" stop-opacity="0"/><stop offset=".74" stop-color="#b7f4ff" stop-opacity=".08"/><stop offset="1" stop-color="#fff" stop-opacity=".33"/>
    </linearGradient>
  </defs>
  <g fill="url(#pane-glint)">
    <path d="M568 260 561 233 565 217 583 219 582 240Z M577 265 604 249 621 235 632 249 632 272 605 266Z M577 272 603 285 621 286 616 307 599 324 582 301Z"/>
    <path d="M445 323 451 298 438 243 481 259 512 256 519 274 519 292 484 317Z M538 355 544 324 569 317 599 324 602 352 633 402 592 387Z"/>
    <path d="M647 230 689 193 722 187 782 145 753 261 708 273 657 265 663 249Z"/>
  </g>
  <g class="fracture-rays" fill="none">${cracks.map(d => `<path pathLength="1" d="${d}"/>`).join('')}</g>
  <path class="fracture-branches" fill="none" d="${branches}"/>
  <path class="fracture-branches" fill="#152331" fill-opacity=".38" d="m554 251 9 1 7 -7 9 3 2 7 11 3 -1 10 6 6 -8 8 -9 2 -8 8 -7 -6 -11 1 -2 -9 -9 -4 3 -8 -2 -6z"/>
  <path fill="#e1f9ff" fill-opacity=".8" d="m564 262 7 -6 -2 8 7 -3 -3 7 8 -1 -8 4 -3 8 -1 -8 -8 3 5 -6 -8 -2z"/>
  <g class="fracture-splinters">
    <path d="m569 253 -8 -18 12 11z M590 257l16 -10 -11 13z M586 281l12 19 -16 -14z M556 287l-12 8 8 -14z M547 265l-14 -7 17 2z"/>
    <path d="m555 243 -10 -25 13 19z M609 278l17 3 -12 2z M574 303l2 17 -6 -12z M538 279l-20 4 15 -7z"/>
  </g>
</svg>`

// Builds every file in assets/brand/ (logos and textures) from the marks.
// Run from the repository root:  node assets/brand/tools/build.mjs
// The letters of "oodle" come from oodle.json (outlines made by outline.py).
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..');
const OODLE = JSON.parse(readFileSync(join(here, 'oodle.json'), 'utf8'));


var THEMES={
  canvas:{n:'Canvas',bg:'#0e1014',fg:'#c8f169'},
  lime:{n:'Lime',bg:'#c8f169',fg:'#1c2a05'},
  paper:{n:'Paper',bg:'#f4f4f1',fg:'#14161b'},
  blue:{n:'Blue',bg:'#3c5aa8',fg:'#c8f169'},
  coral:{n:'Coral',bg:'#ff7a6b',fg:'#14161b'},
  quiet:{n:'Quiet',bg:'#0e1014',fg:'#232a35'}
};
var PATTERNS=[
  {id:'diag',n:'Diagonal wordmark'},
  {id:'tess',n:'N tessellation'},
  {id:'wave',n:'Noodle waves'},
  {id:'wave60',n:'Waves, 60° offset'},
  {id:'wave90',n:'Waves, 90° offset'},
  {id:'wave120',n:'Waves, 120° offset'},
  {id:'lat',n:'N lattice'},
  {id:'latz',n:'N lattice, level diagonals'},
  {id:'tessd',n:'N tessellation, diagonal'},
  {id:'tiles',n:'Tile confetti'}
];
function glyph(cx,cy,s,fg,hole,sw){
  var k=s/60, r=10.5, o='';
  o+='<g transform="translate('+cx+' '+cy+') scale('+k+') translate(-60 -60)">';
  o+='<path d="M30 90 V30 C66 30 54 90 90 90 V30" fill="none" stroke="'+fg+'" stroke-width="'+sw+'" stroke-linecap="round" stroke-linejoin="round"/>';
  [[30,90],[30,30],[90,90],[90,30]].forEach(function(p){
    o+='<circle cx="'+p[0]+'" cy="'+p[1]+'" r="'+r+'" fill="'+fg+'"/><circle cx="'+p[0]+'" cy="'+p[1]+'" r="4" fill="'+hole+'"/>';
  });
  return o+'</g>';
}
function tileIcon(cx,cy,size,rot,fg,bg){
  var o='<g transform="translate('+cx+' '+cy+') rotate('+rot+') scale('+(size/120)+') translate(-60 -60)">';
  o+='<rect x="4" y="4" width="112" height="112" rx="28" fill="'+fg+'"/>';
  o+='<path d="M34 86 V34 C64 34 56 86 86 86 V34" fill="none" stroke="'+bg+'" stroke-width="8" stroke-linecap="round" stroke-linejoin="round"/>';
  [[34,86],[34,34],[86,86],[86,34]].forEach(function(p){
    o+='<circle cx="'+p[0]+'" cy="'+p[1]+'" r="10" fill="'+bg+'"/><circle cx="'+p[0]+'" cy="'+p[1]+'" r="4" fill="'+fg+'"/>';
  });
  return o+'</g>';
}
// "Noodle" with outlined letters. Sized so the N is 36 units tall, as in the page this came from.
function word(x,y,fg,bg,op){
  var k=52/1000, m=OODLE.medium;
  return '<g opacity="'+op+'">'+glyph(x+22,y-18,26,fg,bg,11.5)+
    '<path transform="translate('+(x+46)+' '+y+') scale('+k+')" d="'+m.d+'" fill="'+fg+'"/></g>';
}
/* Shift the content so no node lies on a tile edge, and draw the eight neighbours so wires that cross the edge join up. */
function wrap(b,w,h,sx,sy){
  var o='';
  [-1,0,1].forEach(function(i){[-1,0,1].forEach(function(j){
    o+='<g transform="translate('+(sx+i*w)+' '+(sy+j*h)+')">'+b+'</g>';
  });});
  return o;
}
function def(kind,th){
  var fg=th.fg,bg=th.bg,b='',w,h,t='';
  if(kind==='diag'){
    w=240;h=150;t='rotate(-22)';
    b=word(0,62,fg,bg,1)+word(120,137,fg,bg,.4)+word(-120,137,fg,bg,.4);
  }else if(kind==='tess'){
    w=100;h=100;
    b=glyph(25,25,30,fg,bg,9)+'<g opacity=".4">'+glyph(75,75,30,fg,bg,9)+'</g>';
  }else if(kind==='wave'){
    w=120;h=120;
    var wv='<path d="M0 0 C30 0 30 60 60 60 C90 60 90 0 120 0" fill="none" stroke="'+fg+'" stroke-width="6" stroke-linecap="round"/>'+
      [[0,0],[60,60],[120,0]].map(function(p){return '<circle cx="'+p[0]+'" cy="'+p[1]+'" r="8" fill="'+fg+'"/><circle cx="'+p[0]+'" cy="'+p[1]+'" r="3" fill="'+bg+'"/>';}).join('');
    b=wv+'<g opacity=".4" transform="translate(60 60)">'+wv+'</g><g opacity=".4" transform="translate(-60 60)">'+wv+'</g>';
    b=wrap(b,w,h,30,30);
  }else if(kind==='wave60'||kind==='wave90'||kind==='wave120'){
    var cfg={wave60:[20,6,[1,.75,.55,.4,.28,.2]],wave90:[30,4,[1,.7,.45,.25]],wave120:[40,3,[1,.6,.3]]}[kind],off=cfg[0],rows=cfg[1],ops=cfg[2];
    w=120;h=60*rows;
    var one='<path d="M0 0 C30 0 30 60 60 60 C90 60 90 0 120 0" fill="none" stroke="'+fg+'" stroke-width="6" stroke-linecap="round"/>'+
      [[0,0],[60,60],[120,0]].map(function(p){return '<circle cx="'+p[0]+'" cy="'+p[1]+'" r="8" fill="'+fg+'"/><circle cx="'+p[0]+'" cy="'+p[1]+'" r="3" fill="'+bg+'"/>';}).join('');
    for(var k=0;k<rows;k++){
      [-120,0].forEach(function(dx){
        b+='<g opacity="'+ops[k]+'" transform="translate('+(k*off+dx)+' '+(60*k)+')">'+one+'</g>';
      });
    }
    b=wrap(b,w,h,off/2,30);
  }else if(kind==='latz'){
    w=80;h=80;t='rotate(-45)';
    b='<path d="M0 80 V0 C32 0 48 80 80 80 V0" fill="none" stroke="'+fg+'" stroke-width="5" stroke-linecap="round"/>'+
      [[0,80],[0,0],[80,80],[80,0]].map(function(p){return '<circle cx="'+p[0]+'" cy="'+p[1]+'" r="7" fill="'+fg+'"/><circle cx="'+p[0]+'" cy="'+p[1]+'" r="2.7" fill="'+bg+'"/>';}).join('');
    b=wrap(b,w,h,40,40);
  }else if(kind==='tessd'){
    w=100;h=100;t='rotate(-30)';
    b=glyph(25,25,32,fg,bg,9)+'<g opacity=".4">'+glyph(75,75,32,fg,bg,9)+'</g>';
  }else if(kind==='lat'){
    w=80;h=80;
    b='<path d="M0 80 V0 C32 0 48 80 80 80 V0" fill="none" stroke="'+fg+'" stroke-width="5" stroke-linecap="round"/>'+
      [[0,80],[0,0],[80,80],[80,0]].map(function(p){return '<circle cx="'+p[0]+'" cy="'+p[1]+'" r="7" fill="'+fg+'"/><circle cx="'+p[0]+'" cy="'+p[1]+'" r="2.7" fill="'+bg+'"/>';}).join('');
    b=wrap(b,w,h,40,40);
  }else{
    w=140;h=140;
    b=tileIcon(35,35,52,-14,fg,bg)+'<g opacity=".55">'+tileIcon(105,105,52,14,fg,bg)+'</g>';
  }
  return {w:w,h:h,t:t,b:b};
}
var uid=0;
function svg(kind,th,scale){
  var d=def(kind,th),id='p'+(uid++);
  var tr=(d.t?d.t+' ':'')+'scale('+scale+')';
  return '<svg xmlns="http://www.w3.org/2000/svg" aria-hidden="true"><defs><pattern id="'+id+'" width="'+d.w+'" height="'+d.h+'" patternUnits="userSpaceOnUse" patternTransform="'+tr+'">'+d.b+'</pattern></defs>'+
    '<rect width="100%" height="100%" fill="'+th.bg+'"/><rect width="100%" height="100%" fill="url(#'+id+')"/></svg>';
}

// ---- files -------------------------------------------------------------

const write = (path, text) => { mkdirSync(dirname(join(root, path)), { recursive: true }); writeFileSync(join(root, path), text); };

const PATTERN_FILES = {
  diag: 'diagonal-wordmark', tess: 'n-tessellation', wave: 'waves', wave60: 'waves-60',
  wave90: 'waves-90', wave120: 'waves-120', lat: 'n-lattice', latz: 'n-lattice-level',
  tessd: 'n-tessellation-diagonal', tiles: 'tile-confetti',
};
const W = 1920, H = 1080;
for (const p of PATTERNS) {
  for (const key of Object.keys(THEMES)) {
    let s = svg(p.id, THEMES[key], 1)
      .replace('<svg xmlns="http://www.w3.org/2000/svg" aria-hidden="true">', `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}">`)
      .replace(/width="100%" height="100%"/g, `width="${W}" height="${H}"`);
    write(`textures/${PATTERN_FILES[p.id]}-${key}.svg`, s + '\n');
  }
}

// Logos. Colourways: lime, ink, white, limeink. The N graph has real holes (a mask), so it sits on any background.
const COLOURWAYS = { lime: '#c8f169', ink: '#14161b', white: '#ffffff', limeink: '#1c2a05' };
function wordmark(weight, fg) {
  const o = OODLE[weight], bold = weight === 'bold';
  const box = bold ? 82 : 81, origin = bold ? 19 : 19.5, wire = bold ? 15 : 11.5, r = bold ? 11 : 10.5;
  const k = 700 / box, id = 'h' + weight;
  const nodes = [[30, 90], [30, 30], [90, 90], [90, 30]];
  const n = `<g transform="translate(0 -700) scale(${k}) translate(${-origin} ${-origin})">` +
    `<g mask="url(#${id})"><path d="M30 90 V30 C66 30 54 90 90 90 V30" fill="none" stroke="${fg}" stroke-width="${wire}" stroke-linecap="round" stroke-linejoin="round"/>` +
    nodes.map(p => `<circle cx="${p[0]}" cy="${p[1]}" r="${r}" fill="${fg}"/>`).join('') + '</g></g>';
  const mask = `<mask id="${id}" maskUnits="userSpaceOnUse" x="0" y="0" width="120" height="120"><rect width="120" height="120" fill="#fff"/>` +
    nodes.map(p => `<circle cx="${p[0]}" cy="${p[1]}" r="4" fill="#000"/>`).join('') + '</mask>';
  const pad = 40, x0 = -pad, y0 = o.top - pad, w = 750 + o.width + 2 * pad, h = -o.top + 2 * pad + 20;
  // The mask lives in the N's own coordinates, so wrap the N in a group that carries the mask definition.
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="${x0} ${y0} ${w} ${h}" width="${(w / 4).toFixed(0)}" height="${(h / 4).toFixed(0)}">` +
    `<title>Noodle</title><defs>${mask}</defs>${n}<path transform="translate(750 0)" d="${o.d}" fill="${fg}"/></svg>\n`;
}
for (const [name, fg] of Object.entries(COLOURWAYS)) {
  write(`logo/wordmark-${name}.svg`, wordmark('medium', fg));
  write(`logo/wordmark-bold-${name}.svg`, wordmark('bold', fg));
}
write('logo/icon.svg', `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120 120" width="512" height="512"><title>Noodle</title>` +
  `<rect x="4" y="4" width="112" height="112" rx="28" fill="#c8f169"/>` +
  `<path d="M34 86 V34 C64 34 56 86 86 86 V34" fill="none" stroke="#1c2a05" stroke-width="8" stroke-linecap="round" stroke-linejoin="round"/>` +
  [[34, 86], [34, 34], [86, 86], [86, 34]].map(p => `<circle cx="${p[0]}" cy="${p[1]}" r="10" fill="#1c2a05"/><circle cx="${p[0]}" cy="${p[1]}" r="4" fill="#c8f169"/>`).join('') + '</svg>\n');
console.log('done');

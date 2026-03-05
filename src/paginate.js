(function(){
  function parseHash() {
    var h = {};
    location.hash.replace(/^#/,'').split('&').forEach(function(s){
      var p = s.split('='); if (p.length===2) h[p[0]] = parseInt(p[1],10)||1;
    });
    return h;
  }
  function setHash(h) {
    var parts = [];
    for (var k in h) parts.push(k+'='+h[k]);
    location.hash = parts.join('&');
  }
  function paginate(containerId, pagerId, hashKey) {
    var container = document.getElementById(containerId);
    if (!container) return;
    var pages = container.querySelectorAll('.page');
    if (!pages.length) return;
    var total = pages.length;
    function show(p) {
      p = Math.max(1, Math.min(p, total));
      for (var i = 0; i < pages.length; i++)
        pages[i].style.display = (i === p - 1) ? '' : 'none';
      var h = parseHash(); h[hashKey] = p; setHash(h);
      var pager = document.getElementById(pagerId);
      pager.innerHTML = '';
      if (p > 1) {
        var prev = document.createElement('a');
        prev.href = '#'; prev.textContent = '\u2190 Prev';
        prev.onclick = function(e){ e.preventDefault(); show(p - 1); };
        pager.appendChild(prev);
      }
      if (total > 1) {
        var span = document.createElement('span');
        span.textContent = ' Page ' + p + ' of ' + total + ' ';
        pager.appendChild(span);
      }
      if (p < total) {
        var next = document.createElement('a');
        next.href = '#'; next.textContent = 'Next \u2192';
        next.onclick = function(e){ e.preventDefault(); show(p + 1); };
        pager.appendChild(next);
      }
    }
    show(parseHash()[hashKey] || 1);
    window.addEventListener('hashchange', function(){ show(parseHash()[hashKey] || 1); });
  }
  paginate('main-entries','pager','page');
  paginate('noisy-entries','noisy-pager','noisy');
})();

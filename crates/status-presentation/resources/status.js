function open_close(myid) {
  var el = document.getElementById(myid);
  if (el.style.display == "none" || el.style.display == '') {
    el.style.display = 'block';
  } else {
    el.style.display = 'none';
  }
}

function open_stratum0() {
  open_close('stratum0')
}

function open_stratum1() {
  open_close('stratum1')
}

function open_repositories() {
  open_close('repositories')
}

function open_syncservers() {
  open_close('syncservers')
}

document.addEventListener('DOMContentLoaded', function () {
  addClickHandler('stratum0_handler', open_stratum0);
  addClickHandler('stratum1_handler', open_stratum1);
  addClickHandler('repositories_handler', open_repositories);
  addClickHandler('syncservers_handler', open_syncservers);
  renderHistoryBars();
});

function addClickHandler(id, handler) {
  var el = document.getElementById(id);
  if (el) el.addEventListener('click', handler);
}

function renderHistoryBars() {
  var holder = document.getElementById('history-bars');
  if (!holder) return;
  var url = holder.getAttribute('data-history-url') || window.STATUS_HISTORY_URL || 'history.json';
  fetch(url)
    .then(function (r) {
      if (!r.ok) throw new Error('history unavailable');
      return r.json();
    })
    .then(function (history) {
      var servers = history.servers || {};
      var names = Object.keys(servers).sort();
      if (names.length === 0) {
        holder.innerHTML = '<p class="muted">No history yet.</p>';
        return;
      }
      holder.innerHTML = names.map(function (name) {
        var server = servers[name];
        var pct = server.uptime ? (server.uptime.pct_90d * 100).toFixed(1) : '0.0';
        return '<div class="history-server"><div class="history-title"><span>' +
          escapeHtml(name) + '</span><span>' + pct + '% · 90d</span></div>' +
          barsSvg(server.bars || []) + '</div>';
      }).join('');
    })
    .catch(function () {
      holder.innerHTML = '<p class="muted">History unavailable.</p>';
    });
}

function barsSvg(bars) {
  var width = 720;
  var height = 24;
  var gap = 1;
  var barWidth = Math.max(2, Math.floor((width - gap * Math.max(0, bars.length - 1)) / Math.max(1, bars.length)));
  var rects = bars.map(function (bar, idx) {
    var cls = 'history-bar ' + statusClass(bar.s);
    var x = idx * (barWidth + gap);
    var title = escapeHtml(bar.d + ' · ' + bar.s + ' · transitions ' + bar.transitions);
    return '<rect class="' + cls + '" x="' + x + '" y="2" width="' + barWidth + '" height="20" rx="1"><title>' + title + '</title></rect>';
  }).join('');
  return '<svg class="history-svg" viewBox="0 0 ' + width + ' ' + height + '" preserveAspectRatio="none">' + rects + '</svg>';
}

function statusClass(status) {
  switch (status) {
    case 'OK': return 'status-ok-fill';
    case 'DEGRADED': return 'status-degraded-fill';
    case 'WARNING': return 'status-warning-fill';
    case 'FAILED': return 'status-failed-fill';
    case 'MAINTENANCE': return 'status-maintenance-fill';
    default: return 'status-nodata-fill';
  }
}

function escapeHtml(value) {
  return String(value).replace(/[&<>"']/g, function (ch) {
    return ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#039;' })[ch];
  });
}

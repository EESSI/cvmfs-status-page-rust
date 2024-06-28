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
  document.getElementById('stratum0_handler')
    .addEventListener('click', open_stratum0);
  document.getElementById('stratum1_handler')
    .addEventListener('click', open_stratum1);
  document.getElementById('repositories_handler')
    .addEventListener('click', open_repositories);
  document.getElementById('syncservers_handler')
    .addEventListener('click', open_syncservers);
});
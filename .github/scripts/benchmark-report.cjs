// Summarize every matrix job, including failures and missing artifacts.
module.exports = async ({github, context, core}) => {
  const params = {...context.repo, run_id: context.runId};
  const jobs = await github.paginate(github.rest.actions.listJobsForWorkflowRun, {...params, filter: 'latest', per_page: 100});
  const artifacts = await github.paginate(github.rest.actions.listWorkflowRunArtifacts, {...params, per_page: 100});
  const escape = value => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  const link = (url, label) => `<a href="${escape(url)}">${escape(label)}</a>`;
  await core.summary.addHeading('Benchmark report')
    .addRaw('Case failures remain failures. Missing evidence is not a passing result. Reports and any videos are linked below.\n')
    .addTable([
      [{data:'Job / case',header:true},{data:'Result',header:true},{data:'Duration',header:true}],
      ...jobs.filter(j => j.name !== 'report').map(j => [link(j.html_url,j.name), escape(j.conclusion || j.status),
        j.completed_at && j.started_at ? `${Math.round((Date.parse(j.completed_at)-Date.parse(j.started_at))/1000)} s` : '—'])
    ]).addHeading('Evidence', 2)
    .addList(artifacts.filter(a => !a.expired).map(a => link(`${context.serverUrl}/${context.repo.owner}/${context.repo.repo}/actions/runs/${context.runId}/artifacts/${a.id}`,a.name)))
    .addRaw(artifacts.length ? '' : 'No artifacts were published. Inspect the failed jobs above.\n').write();
};

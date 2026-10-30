#!/usr/bin/env python3
"""Linux GIS validation and reproduction. Download only with explicit --download-pinned."""
import argparse, hashlib, json, os, platform, resource, shlex, shutil, uuid
import signal, statistics, subprocess, sys, tempfile, time, urllib.request
from pathlib import Path

PROJECTION = 'select(.properties.type=="Major Highway") | .properties.sov_a3'
EVENTS = 'inputs | select(.[0][0]=="features")'
# truncate_stream drops empty containers/scalars at the truncation boundary.
RECON = ('fromstream((' + EVENTS + ') | . as $e | '
         'if length==2 and (.[0]|length)==2 then [[],.[1]] '
         'else 2|truncate_stream($e) end)')
FUSED = ('fromstream((' + EVENTS + ') | '
         '(select((.[0][2]=="properties" and (.[0][3]=="type" or .[0][3]=="sov_a3")) or '
         '(length==1 and (.[0]|length)==3) or (length==2 and (.[0]|length)==2))) | '
         '. as $e | if length==2 and (.[0]|length)==2 then [[],.[1]] '
         'else 2|truncate_stream($e) end) | ' + PROJECTION)
BAD = 'fromstream(2|truncate_stream(' + EVENTS + '))'
CAP = 128 * 1024 * 1024


def digest(path):
    h = hashlib.sha256(); lines = size = 0
    with Path(path).open('rb') as f:
        while b := f.read(1048576):
            h.update(b); lines += b.count(b'\n'); size += len(b)
    return dict(sha256=h.hexdigest(), lines=lines, bytes=size)


def jsonbytes(v):
    return json.dumps(v, ensure_ascii=False, separators=(',', ':')).encode()


def oracle(features):
    h = hashlib.sha256(); n = 0
    for f in features:
        if isinstance(f, dict) and isinstance(f.get('properties'), dict):
            p = f['properties']
            if p.get('type') == 'Major Highway':
                h.update(jsonbytes(p.get('sov_a3')) + b'\n'); n += 1
    return dict(sha256=h.hexdigest(), lines=n)


def matches(got, expected):
    return all(got[k] == expected[k] for k in ('sha256', 'lines'))


class Runner:
    def __init__(self, out, timeout, prior_run=None):
        self.out = out; self.timeout = timeout; self.records = []
        self.prior_run = prior_run
        self.raw = (out / 'raw.jsonl').open('w')

    def add(self, r):
        self.records.append(r); self.raw.write(json.dumps(r, ensure_ascii=True) + '\n'); self.raw.flush()
        return r

    def run(self, label, commands, expected=None, cap=None, **extra):
        with tempfile.TemporaryDirectory(prefix='.gis-', dir=self.out) as td:
            td = Path(os.path.relpath(td)); processes = []; errors = []; timed = []; expired = False
            output = td / 'stdout'; start = time.monotonic(); deadline = start + self.timeout
            with output.open('wb') as sink:
                prior = None
                try:
                    for i, command in enumerate(commands):
                        err = (td / ('stderr%d' % i)).open('wb'); errors.append(err)
                        tf = td / ('time%d' % i)
                        argv = ['/usr/bin/time', '-f', '%e\t%M\t%x', '-o', str(tf)] + command
                        def limit():
                            resource.setrlimit(resource.RLIMIT_AS, (cap, cap))
                        p = subprocess.Popen(argv, stdin=prior if prior is not None else subprocess.DEVNULL, stdout=sink if i==len(commands)-1 else subprocess.PIPE,
                                             stderr=err, start_new_session=True,
                                             preexec_fn=limit if cap else None)
                        if prior is not None: prior.close()
                        prior = p.stdout; processes.append(p); timed.append((tf, argv))
                    for p in processes:
                        p.wait(timeout=max(.01, deadline-time.monotonic()))
                except subprocess.TimeoutExpired:
                    expired = True
                    for p in processes:
                        try: os.killpg(p.pid, signal.SIGKILL)
                        except ProcessLookupError: pass
                    for p in processes: p.wait()
                except BaseException:
                    for p in processes:
                        try: os.killpg(p.pid, signal.SIGTERM)
                        except ProcessLookupError: pass
                    for p in processes:
                        try: p.wait(timeout=2)
                        except subprocess.TimeoutExpired:
                            try: os.killpg(p.pid, signal.SIGKILL)
                            except ProcessLookupError: pass
                            p.wait()
                    raise
                finally:
                    for e in errors: e.close()
            elapsed = time.monotonic()-start; stages = []
            for i, (p, (tf, argv)) in enumerate(zip(processes, timed)):
                rawtime = tf.read_text() if tf.exists() else ''
                metric = None
                try:
                    a,b,c = rawtime.strip().splitlines()[-1].split('\t')
                    metric = dict(elapsed_s=float(a), peak_rss_kib=int(b), time_exit=int(c))
                except (ValueError, IndexError): pass
                stages.append(dict(command=shlex.join(commands[i]), argv=commands[i],
                                   measured_command=shlex.join(argv), returncode=p.returncode,
                                   metrics=metric, time_raw=rawtime,
                                   stderr=(td / ('stderr%d'%i)).read_text(errors='replace')))
            got = digest(output)
            success = not expired and len(stages)==len(commands) and all(s['returncode']==0 and s['metrics'] is not None for s in stages)
            r = dict(kind=label, stages=stages, pipeline_command=' | '.join(shlex.join(c) for c in commands), wall_elapsed_s=elapsed, stdout=got,
                     timeout=expired, per_child_rlimit_as_bytes=cap,
                     expected=expected, oracle_match=matches(got,expected) if expected else None,
                     success=success, **extra)
            return self.add(r)


def arms(binary, path):
    p = str(path)
    return {
        'ordinary_jq': [['jq','-c','.features[] | '+PROJECTION,p]],
        'binary_jq': [[str(binary),'--array-field','features',p],['jq','-c',PROJECTION]],
        'streaming_jq': [['jq','-cn','--stream',FUSED,p]],
    }


def selfcheck(r, binary, directory):
    feature = lambda name: {'properties': {'type':'Major Highway','sov_a3':name}}
    features = [feature('ONE'), {'geometry': {'coordinates': [[[1,2],[3,4]],[]]},
                 'properties': {'sov_a3':'TWO','type':'Major Highway'}}, {},
                {'properties': {'type':'Other','sov_a3':'NO'}}, feature('THREE')]
    features[0]['geometry'] = {'coordinates': [[1,2],[3,4]]}
    fixture = {'before': {'features':[feature('WRONG')]}, 'features':features,
               'after': {'features':[feature('WRONG2')]}}
    path=directory/'fixture.json'; path.write_bytes(jsonbytes(fixture))
    sequence = b''.join(jsonbytes(f)+b'\n' for f in features)
    expected_sequence = dict(sha256=hashlib.sha256(sequence).hexdigest(),lines=len(features))
    bad=r.run('recipe_attempt', [['jq','-cn','--stream',BAD,str(path)]], expected_sequence,
              recipe='initial truncation loses empty object', expected_failure=True)
    if not bad['success'] or bad['oracle_match'] is not False:
        raise RuntimeError('negative-control recipe did not execute successfully with wrong output')
    checks=[]
    checks.append(r.run('reconstruction_check', [['jq','-cn','--stream',RECON,str(path)]], expected_sequence))
    # Test every JSON element shape, including boundary events lost by naive truncation.
    mixed=directory/'mixed.json'; values=[{},[],None,7,'text',{'nested':[[],{},[1,2]]}]
    mixed.write_bytes(jsonbytes({'features':values,'tail':[]}))
    seq=b''.join(jsonbytes(v)+b'\n' for v in values)
    checks.append(r.run('reconstruction_all_element_shapes', [['jq','-cn','--stream',RECON,str(mixed)]],
                        dict(sha256=hashlib.sha256(seq).hexdigest(),lines=len(values))))
    for name, cmds in arms(binary,path).items():
        checks.append(r.run('fixture_projection',cmds,oracle(features),arm=name))
    token = (b'{"features":[ { "huge":1.234567890123456789e+9999, '
             b'"integer":1234567890123456789012345678901234567890, '
             b'"dup":1,"dup":2,"escaped":"\\u0041\\/\\n\\\\\\\"" } ],"after":true}')
    # Explicit expected lexemes; binary retains internal spaces on this one-line fixture.
    token_expected=(b'{ "huge":1.234567890123456789e+9999, '
                    b'"integer":1234567890123456789012345678901234567890, '
                    b'"dup":1,"dup":2,"escaped":"\\u0041\\/\\n\\\\\\\"" }\n')
    tp=directory/'tokens.json'; tp.write_bytes(token)
    checks.append(r.run('raw_literal_preservation',[[str(binary),'--array-field','features',str(tp)]],
                        dict(sha256=hashlib.sha256(token_expected).hexdigest(),lines=1),
                        fixture=token.decode(), expected_literal=token_expected.decode()))
    cases = [('truncated',b'{"features":[{"ok":1}]'),
        ('malformed_skipped_after',b'{"features":[{"ok":1}],"tail":[1,]}'),
        ('invalid_utf8_outside',b'{"features":[{"ok":1}],"tail":"\xff"}'),
        ('duplicated',b'{"features":[],"features":[]}'),('missing',b'{"other":[]}'),
        ('non_array',b'{"features":{}}')]
    cases += [('valid',b'{"features":[{"ok":1}]}')]
    for name, contents in cases:
        src=directory/(name+'.json'); src.write_bytes(contents)
        dest=directory/(name+'.jsonl'); sentinel=b'SENTINEL\n'
        for existing, exitcode in ((False,0 if name=='valid' else 2),(True,73)):
            if existing: dest.write_bytes(sentinel)
            else: dest.unlink(missing_ok=True)
            before={p.name for p in directory.iterdir()}
            label='atomic_output_existing' if existing else 'atomic_output_absent'
            rec=r.run(label,[[str(binary),'--array-field','features','--output',str(dest),str(src)]],case=name)
            exists=dest.exists(); unchanged=exists and dest.read_bytes()==sentinel
            content_ok=exists and dest.read_bytes()==b'{"ok":1}\n'
            leftovers={p.name for p in directory.iterdir()}-before
            if not existing and exitcode==0: leftovers.discard(dest.name)
            passed=(rec['stages'][0]['returncode']==exitcode and not rec['timeout'] and
                    (unchanged if existing else content_ok if exitcode==0 else not exists) and
                    rec['stdout']['bytes']==0 and not leftovers)
            checks.append(dict(atomic_pass=passed))
            r.add(dict(kind='atomic_assertion',case=name,existing=existing,expected_exit=exitcode,
                actual_exit=rec['stages'][0]['returncode'],output_exists=exists,
                sentinel_unchanged=unchanged if existing else None,valid_content_match=content_ok,
                leftover_files=sorted(leftovers),passed=passed))
    passed=all(c.get('atomic_pass', c.get('success') and c.get('oracle_match')) for c in checks)
    return dict(passed=passed, intentionally_failed_recipe_recorded=bad['success'] and bad['oracle_match'] is False)


def summarize(records):
    rows=[]
    for dataset in ('roads','synthetic_4x'):
        for arm in ('ordinary_jq','binary_jq','streaming_jq'):
            rs=[r for r in records if r['kind']=='timing' and r['dataset']==dataset and r['arm']==arm]
            if not rs: continue
            stages=[]
            for i in range(len(rs[0]['stages'])):
                metrics=[r['stages'][i]['metrics'] for r in rs]
                stages.append(dict(stage=i, command=rs[0]['stages'][i]['command'],
                    peak_rss_kib_each=[m['peak_rss_kib'] if m else None for m in metrics],
                    median_peak_rss_kib=statistics.median([m['peak_rss_kib'] for m in metrics if m]) if any(metrics) else None,
                    elapsed_s_each=[m['elapsed_s'] if m else None for m in metrics]))
            rows.append(dict(dataset=dataset,arm=arm,rounds=len(rs),
                median_wall_elapsed_s=statistics.median(r['wall_elapsed_s'] for r in rs),stages=stages,
                all_success_and_match=all(r['success'] and r['oracle_match'] for r in rs)))
    return rows


def print_report(result):
    print(result['status'] + ': GIS validation')
    if result.get('error'): print(result['error'])
    print('Binary SHA-256:', result.get('binary',{}).get('actual',{}).get('sha256','unavailable'))
    for item in result.get('inputs',[]):
        print('%s: %d bytes, %d features, %d selected lines; SHA-256 %s' %
              (item['dataset'],item['fingerprint']['bytes'],item['features'],item['oracle']['lines'],item['fingerprint']['sha256']))
    if result.get('measurements'):
        print('Three rotated rounds; wall elapsed medians; separate per-process RSS medians (KiB):')
    for row in result.get('measurements',[]):
        print('%s / %s: %.3f s; RSS %s; %s' % (row['dataset'],row['arm'],row['median_wall_elapsed_s'],
              ', '.join(str(s['median_peak_rss_kib']) for s in row['stages']),
              'PASS' if row['all_success_and_match'] else 'FAIL'))
    if result.get('memory_cap'):
        print('128 MiB RLIMIT_AS per child, separate from timings:')
    for row in result.get('memory_cap',[]):
        print('%s / %s: exits %s; oracle %s' % (row['dataset'],row['arm'],
              [s['returncode'] for s in row['stages']],row['oracle_match']))
    print('Self-check:', result.get('self_check'))
    print('Reproduce offline:')
    print('python3 benchmarks/gis_validation.py --binary .work/package/json-stream-0.2.0-alpha.2-linux-x86_64/json-stream --self-check --out-dir .work/gis-selfcheck')
    print('python3 benchmarks/gis_validation.py --binary .work/package/json-stream-0.2.0-alpha.2-linux-x86_64/json-stream --input-dir .work/gis-inputs --out-dir .work/gis-results --timeout 120')
    print('Exact stage commands, statuses, digests and failed attempts: raw.jsonl; metadata and summaries: results.json.')
    print('Caveats: one host, warm cache, rotated order; only jq compared (GDAL/ijson absent).')
    print('No CPU-efficiency claim; CPU accounting was not collected.')
    print('Per-stage RSS peaks are separate, never summed; RLIMIT_AS is virtual memory per process, not a container limit.')
    print('Scalar digests do not prove raw literal preservation; the explicit token fixture tests it separately.')
    print('Atomic publication was observed after process exit; crash durability is untested.')


def main():
    ap=argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--binary',default='json-stream-0.2.0-alpha.2-linux-x86_64/json-stream')
    ap.add_argument('--input-dir',default='inputs'); ap.add_argument('--out-dir',default='.')
    ap.add_argument('--self-check',action='store_true'); ap.add_argument('--download-pinned',action='store_true')
    ap.add_argument('--timeout',type=float,default=120)
    args=ap.parse_args()
    # Relative paths only: no home/host identity enters reports.
    root=Path.cwd().resolve()
    def local(s):
        p=Path(s).resolve(); p.relative_to(root); return Path(os.path.relpath(p,root))
    out=local(args.out_dir)
    out.mkdir(parents=True,exist_ok=True)
    prior_run=None; archive_error=None
    old=[out/name for name in ('raw.jsonl','results.json') if (out/name).exists()]
    if old:
        archive=out/('prior-run-'+time.strftime('%Y%m%dT%H%M%SZ',time.gmtime())+'-'+uuid.uuid4().hex[:8])
        archive.mkdir()
        try:
            for path in old: path.replace(archive/path.name)
        except Exception:
            for path in old:
                saved=archive/path.name
                if saved.exists(): saved.replace(path)
            archive.rmdir()
            archive_error='prior-run archive failed'
        prior_run=str(archive)
    result={'schema':1,'status':'RUNNING','timestamp_utc':time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime()),
            'date_utc':time.strftime('%Y-%m-%d',time.gmtime()),
            'source':{'path':Path(__file__).name},
            'raw_path':str(out/'raw.jsonl'),'prior_run':prior_run,
            'limitations':['No CPU efficiency claim; CPU accounting was not collected.']}
    result_path=out/'results.json'
    result_path.write_text(json.dumps(result,indent=2)+'\n')
    r=None
    try:
        result['source']['sha256']=digest(__file__)['sha256']
        r=Runner(out,args.timeout,prior_run)
        if archive_error: raise RuntimeError(archive_error)
        binary=local(args.binary); inp=local(args.input_dir)
        # Popen needs an explicit slash for an executable in the current directory.
        binary_arg=str(binary) if '/' in str(binary) else './'+str(binary)
        inp.mkdir(parents=True,exist_ok=True)
        manifest=json.loads((binary.parent/'MANIFEST.json').read_text())
        actual=digest(binary); expected=manifest['files']['json-stream']
        verified=actual['sha256']==manifest['binary_sha256']==expected['sha256'] and actual['bytes']==expected['bytes']
        result['binary']=r.add(dict(kind='binary_verification',path=str(binary),actual=actual,expected=expected,passed=verified))
        if not verified: raise RuntimeError('binary manifest mismatch')
        versions={}
        for name, cmd in [('jq',['jq','--version']),('binary',[binary_arg,'--version']),('time',['/usr/bin/time','--version'])]:
            v=subprocess.run(cmd,capture_output=True,timeout=10)
            versions[name]=dict(command=shlex.join(cmd),returncode=v.returncode,stdout=v.stdout.decode(errors='replace').splitlines()[0])
        result['versions']=versions
        result['host']=dict(os=platform.system(),architecture=platform.machine(),python=platform.python_version(),libc=platform.libc_ver(),logical_cpus=os.cpu_count())
        with tempfile.TemporaryDirectory(prefix='.gis-check-',dir=out) as td:
            result['self_check']=selfcheck(r,binary_arg,Path(os.path.relpath(td)))
        if not result['self_check']['passed']: raise RuntimeError('self-check failed; no timings performed')
        if not args.self_check:
            meta=json.loads((Path(__file__).resolve().parent/'field-corpora.json').read_text())['ne-roads']; roads=inp/'roads.geojson'
            if not roads.exists():
                if not args.download_pinned: raise RuntimeError('roads missing: use --download-pinned for the single authorized download')
                with tempfile.NamedTemporaryFile(prefix='.roads-',dir=inp,delete=False) as f:
                    temp=Path(f.name)
                    try:
                        with urllib.request.urlopen(meta['url'],timeout=60) as response:
                            shutil.copyfileobj(response,f)
                    except BaseException:
                        temp.unlink(missing_ok=True); raise
                d=digest(temp)
                if d['bytes']!=meta['bytes'] or d['sha256']!=meta['sha256']:
                    temp.unlink(); raise RuntimeError('download integrity mismatch')
                temp.replace(roads)
            d=digest(roads)
            r.add(dict(kind='corpus_integrity',path=str(roads),actual=d,expected_sha256=meta['sha256'],expected_bytes=meta['bytes']))
            if d['bytes']!=meta['bytes'] or d['sha256']!=meta['sha256']: raise RuntimeError('roads integrity mismatch')
            with roads.open() as f: features=json.load(f)['features']
            if len(features)!=meta['n_elements']: raise RuntimeError('unexpected feature count')
            expected_roads=oracle(features)
            if expected_roads['lines']!=meta['expected_count']: raise RuntimeError('unexpected projection count')
            synthetic=inp/'roads.synthetic-4x.geojson'
            with tempfile.TemporaryFile(dir=inp) as fragment:
                for i,f in enumerate(features):
                    if i: fragment.write(b',')
                    fragment.write(jsonbytes(f))
                with synthetic.open('wb') as dest:
                    dest.write(b'{"type":"FeatureCollection","features":[')
                    for i in range(4):
                        if i: dest.write(b',')
                        fragment.seek(0); shutil.copyfileobj(fragment,dest)
                    dest.write(b']}\n')
            # Reparse the actual generated input independently before any benchmarks.
            del features
            with synthetic.open() as f: enlarged=json.load(f)['features']
            expected_synthetic=oracle(enlarged); synthetic_count=len(enlarged); del enlarged
            result['inputs']=[dict(dataset='roads',path=str(roads),fingerprint=d,features=meta['n_elements'],oracle=expected_roads),
                dict(dataset='synthetic_4x',path=str(synthetic),fingerprint=digest(synthetic),features=synthetic_count,
                     oracle=expected_synthetic,derivation='4 ordered repetitions of roads features; Python compact JSON, UTF-8, FeatureCollection root')]
            datasets=[('roads',roads,expected_roads),('synthetic_4x',synthetic,expected_synthetic)]
            for name,path,expected_output in datasets:
                for arm,cmds in arms(binary_arg,path).items():
                    rec=r.run('preflight',cmds,expected_output,dataset=name,arm=arm)
                    if not rec['success'] or not rec['oracle_match']: raise RuntimeError('preflight failure: '+name+'/'+arm)
            # All oracle checks precede all timings. No benchmark jobs overlap.
            for name,path,expected_output in datasets:
                recipes=arms(binary_arg,path); names=list(recipes)
                for roundno in range(3):
                    order=names[roundno:]+names[:roundno]
                    for arm in order:
                        r.run('timing',recipes[arm],expected_output,dataset=name,arm=arm,round=roundno+1,order=order)
            for name,path,expected_output in datasets:
                for arm,cmds in arms(binary_arg,path).items():
                    r.run('memory_cap',cmds,expected_output,cap=CAP,dataset=name,arm=arm)
            result['measurements']=summarize(r.records)
            result['memory_cap']=[v for v in r.records if v['kind']=='memory_cap']
        result['limitations']=[
            'No CPU-efficiency claim; CPU accounting was not collected.',
            'One host; warm cache from oracle/preflight reads; three rotated sequential rounds; no cold-cache or statistical generalization.',
            'Only jq compared; GDAL and ijson absent from this evaluation; no package installation.',
            '128 MiB RLIMIT_AS is a per-child-process virtual address space cap, not container-wide memory or RSS.',
            'Per-stage peak RSS is reported separately; peaks are never summed and are not simultaneous pipeline RSS.',
            'Scalar digest equality does not establish raw token fidelity; explicit token fixture tested separately.',
            'Atomic publication cases observe exit/output state after completion; crash durability and every filesystem race are untested.',
            'jq streaming is a filtered projection; full reconstruction is separately verified on tiny fixtures.']
        required=[v for v in r.records if v['kind'] in ('preflight','timing')]
        result['status']='PASS' if (all(v['success'] and v['oracle_match'] for v in required) and all(not v['success'] or v['oracle_match'] for v in r.records if v['kind']=='memory_cap')) else 'FAIL'
    except Exception as e:
        result['status']='FAIL'; result['error']=str(e)
        if r: r.add(dict(kind='fatal',error=str(e)))
    finally:
        result['raw_records']=len(r.records) if r else 0
        result_path.write_text(json.dumps(result,indent=2)+'\n')
        if r: r.raw.close()
    print_report(result)
    return 0 if result['status']=='PASS' else 1

if __name__=='__main__':
    raise SystemExit(main())

import hashlib, json, os, subprocess, sys, tempfile, time
from pathlib import Path

target=Path(os.environ['PR416_TARGET_DIR']).resolve()
control=Path(os.environ['PR416_CONTROL_DIR']).resolve()
diagnostic=Path(os.environ['PR416_DIAGNOSTIC_DIR']).resolve()
expected=os.environ['PR416_EXPECTED_SHA']
root=Path(os.environ['PR416_RUN_DIR']).resolve()
root.mkdir(parents=True,exist_ok=True)
sys.path.insert(0,str(control/'scripts'))
import hosted_file_tabs_gui as tabs
gv=tabs.load_module(control,'scripts/gui_validate.py','pr416_gv')
inter=tabs.load_module(control,'scripts/hosted_gui_interaction.py','pr416_inter')
env=gv.RealEnvironment()
steps=[]; scenarios=[]; build={}; target_info={}; holders=[]
def step(name,result,**kw):
    item=dict(name=name,result=result,**kw);steps.append(item);return item
def call(args):
    ok,out,err=inter.run_helper(helper,[str(a) for a in args],20)
    if not ok: raise RuntimeError(err)
    return json.loads(out)
def capture(label,window=None):
    result=inter.capture_named(gv,env,config,window or window_id,case,label)
    steps.append(result)
    if result['result']!='pass': raise RuntimeError(result.get('reason','capture failed'))
    return case/(label+'.png')
def visible(pattern,region='body'):
    answer=call(['find',image,pattern,region]);step('visible_'+pattern,'pass' if answer['found'] else 'fail',observation=answer)
    if not answer['found']: raise AssertionError('expected visible text missing: '+pattern)
def action(name,args):
    observation=call(args);step(name,'pass',observation=observation)
    return capture(name)
def click(name,pattern,region,button='left'):
    return action(name,['click',pid,image,pattern,region,button])

def expect_bytes(name,path,expected_bytes):
    deadline=time.monotonic()+8
    while time.monotonic()<deadline:
        actual=path.read_bytes()
        if actual==expected_bytes:break
        time.sleep(0.1)
    passed=actual==expected_bytes
    step(name,'pass' if passed else 'fail',expected_utf8=expected_bytes.decode(),actual_utf8=actual.decode(),sha256=hashlib.sha256(actual).hexdigest())
    if not passed:raise AssertionError('native caret/source probe mismatch: '+name)

def probe_caret_at_end(name,path,original):
    # No caret repositioning here: the actual native key lands wherever the
    # preceding interaction left it. Disk bytes are independently observed.
    probe_image=action(name+'_type',['key',pid,'probe'])
    expect_bytes(name+'_exact_append',path,original+b'q')
    probe_image=action(name+'_undo',['key',pid,'undo'])
    expect_bytes(name+'_exact_restore',path,original)
    return probe_image
try:
    env.acquire_execution()
    config=inter.make_config(gv,workspace_dir=target,scenario='pr416-preflight',expected_sha=expected,request_id='pr416-'+os.environ['GITHUB_RUN_ID'],generation='0',run_dir=root/'preflight',fixture_path=None,features=['timing-probe'],extra_env={},startup_timeout=15,window_timeout=30)
    pre,target_info=gv.do_preflight(env,config);steps.append(pre)
    if pre['result']!='pass': raise RuntimeError(pre.get('reason','preflight failed'))
    helper_dir=Path(tempfile.mkdtemp(prefix='pr416-mac-helper-'))
    (helper_dir/'actions').mkdir()
    (helper_dir/'hover').mkdir()
    helper=inter.prepare_helper(diagnostic/'pr416-tab-actions.swift',helper_dir/'actions')
    step('prepare_action_helper','pass',sha256=helper.digest)
    old_helper=inter.prepare_helper(control/'scripts/hosted_file_tabs_gui.swift',helper_dir/'hover')
    built,binary,build=gv.do_build(env,config,target_info['actual_sha']);steps.append(built)
    if built['result']!='pass' or binary is None: raise RuntimeError('build failed')
    old=tabs.run_focused_scenario(gv,inter,env,target,old_helper,root,binary,expected,'pr416-'+os.environ['GITHUB_RUN_ID'],15,30,20,gv.RESULT_PRIORITY)
    scenarios.append(old)
    case=root/'pr416_tab_actions';case.mkdir(parents=True,exist_ok=True)
    folder=case/'資料 folder with spaces';folder.mkdir(exist_ok=True)
    first=folder/'first-note 日本語.md';second=folder/'second-note 日本語.md'
    originals={first:b'# Alpha note\n\nAlpha unique body.\n',second:b'# Beta note\n\nBeta unique body.\n'}
    for p,b in originals.items():p.write_bytes(b)
    config=inter.make_config(gv,workspace_dir=target,scenario='pr416-tab-actions',expected_sha=expected,request_id='pr416-'+os.environ['GITHUB_RUN_ID'],generation='1',run_dir=case,fixture_path=folder,features=['timing-probe'],extra_env={},startup_timeout=15,window_timeout=30)
    holder={'process':None};holders.append(holder)
    opened,window_id=inter.open_session(gv,env,config,binary,holder,'initial',old_helper,20);steps.extend(opened)
    pid=inter.current_pid(holder)
    if pid is None or window_id is None:raise RuntimeError('launch/window failed')
    image=capture('folder_first');visible('Alpha.*body')
    image=click('open_second_sidebar','second-note','sidebar');visible('Beta.*body')
    image=click('focus_second_editor','Beta.*body','body');visible('Beta.*body')
    image=action('caret_to_document_end',['key',pid,'end'])
    image=probe_caret_at_end('baseline_native_caret',second,originals[second])
    image=action('hover_before_inactive_right',['hover',pid,image,'first-note','tab'])
    image=click('inactive_first_context','first-note','tab','right')
    image=action('clear_hover_before_copy',['move',pid])
    image=click('copy_inactive_first','フルパス','all')
    clip=call(['clipboard'])['text'];step('clipboard_exact_inactive_path','pass' if clip==str(first) else 'fail',expected=str(first),actual=clip)
    if clip!=str(first):raise AssertionError('clipboard mismatch')
    visible('Beta.*body')
    image=probe_caret_at_end('caret_after_inactive_copy',second,originals[second])
    image=action('hover_before_left',['hover',pid,image,'first-note','tab'])
    image=click('select_first_with_tooltip','first-note','tab')
    image=action('clear_hover_after_left',['move',pid]);visible('Alpha.*body')
    image=action('next_from_selected_first',['key',pid,'next']);visible('Beta.*body')
    for name,key,body in [('next_wrap','next','Alpha.*body'),('next_forward','next','Beta.*body'),('previous_backward','previous','Alpha.*body'),('previous_wrap','previous','Beta.*body')]:
        image=action(name,['key',pid,key]);visible(body)
    image=probe_caret_at_end('caret_after_control_tab_cycle',second,originals[second])
    image=click('inactive_first_reveal_menu','first-note','tab','right')
    image=action('clear_hover_before_reveal',['move',pid])
    image=click('reveal_inactive_first','Finder','all')
    finder=None
    for _ in range(10):
        finder=call(['finder'])
        if finder['paths']==[str(first)] and finder['active']:break
        time.sleep(0.3)
    passed=finder['paths']==[str(first)] and finder['active']
    step('finder_exact_selected_file','pass' if passed else 'fail',expected=[str(first)],observation=finder)
    if not passed or not finder['windows']:raise AssertionError('Finder did not select expected file with a visible window')
    for index,w in enumerate(finder['windows']):capture('finder_selected_'+str(index),str(w))
    image=action('return_from_finder',['move',pid]);visible('Beta.*body')
    image=probe_caret_at_end('caret_after_inactive_reveal',second,originals[second])
    image=action('hover_before_inactive_middle',['hover',pid,image,'first-note','tab'])
    image=click('middle_close_inactive_first','first-note','tab','middle')
    image=action('clear_hover_after_middle',['move',pid]);visible('Beta.*body')
    missing=call(['find',image,'first-note','tab']);step('first_tab_closed_only','pass' if not missing['found'] else 'fail',observation=missing)
    if missing['found']:raise AssertionError('first tab remains')
    image=click('middle_close_last_second','second-note','tab','middle');visible('Untitled','tab')
    image=action('single_tab_control_tab',['key',pid,'next']);visible('Untitled','tab')
    call(['clipboard-set','PR416 disabled path sentinel'])
    image=click('untitled_context','Untitled','tab','right')
    image=click('untitled_disabled_copy','フルパス','all')
    clip=call(['clipboard'])['text'];step('untitled_copy_disabled','pass' if clip=='PR416 disabled path sentinel' else 'fail',actual=clip)
    if clip!='PR416 disabled path sentinel':raise AssertionError('untitled copied a path')
    image=action('dismiss_untitled_context',['key',pid,'escape'])
    for p,b in originals.items():
        actual=p.read_bytes();step('unchanged_'+p.name,'pass' if actual==b else 'fail',sha256=hashlib.sha256(actual).hexdigest())
        if actual!=b:raise AssertionError('fixture changed unexpectedly')
except Exception as exc:
    step('procedure_exception','blocked',reason=str(exc))
finally:
    for holder in holders:
        try:
            proc=holder.get('process')
            if proc is not None:
                proc.terminate();proc.wait(timeout=10)
        except Exception as exc:step('cleanup','blocked',reason=str(exc))
    try:env.release_execution()
    except Exception as exc:step('release','blocked',reason=str(exc))
    outcomes=[s['result'] for s in steps if s.get('result') in gv.RESULT_PRIORITY]+[s['result'] for s in scenarios]
    result=min(outcomes,key=lambda x:gv.RESULT_PRIORITY[x]) if outcomes else 'blocked'
    document=dict(procedure_version='pr416-macos-focused/2',target=target_info,control_sha=subprocess.check_output(['git','-C',str(control),'rev-parse','HEAD'],text=True).strip(),build=build,runner=dict(os='macOS',version=subprocess.check_output(['sw_vers','-productVersion'],text=True).strip(),arch=subprocess.check_output(['uname','-m'],text=True).strip(),image=os.environ.get('ImageVersion')),steps=steps,scenarios=scenarios,overall_result=result,scope_note='Issue411のnative middle/right-click、Control+Tab、Finder選択と、copy/reveal後の本文caretを実キー入力+disk bytes+Undoで確認。旧Issue354 helperのhoverは別scenarioとして区別。SaveAs native raceはWindows実機とcurrent-head regressionを別途確認する。')
    (root/'result.json').write_text(json.dumps(document,ensure_ascii=False,indent=2),encoding='utf-8')
    print(json.dumps(dict(overall_result=result,target=target_info),ensure_ascii=False))
sys.exit(0 if result=='pass' else 2)

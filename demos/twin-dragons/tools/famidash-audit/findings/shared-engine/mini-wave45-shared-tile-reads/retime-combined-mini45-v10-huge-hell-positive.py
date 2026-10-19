from pathlib import Path
import sys,json,gzip,re,hashlib
sys.path.insert(0,'/workspace/harmony-fofii/demos/twin-dragons/tools/famidash-audit');from mesen import SymbolMesen
r=Path('/workspace/famidash-soak');source=r/'current-combined-mini45-v10-hell-clearing.json.gz';w=json.load(gzip.open(source));assert w['purpose']=='clearing-control'and w['trace'][-1]['state']==3
root=Path('/workspace/famidash-current-combined-wave66-mini45-blast-dark-v10-huge-roms');dbg=root/'famidash.dbg';symbols={}
for line in dbg.read_text().splitlines():
 if line.startswith('sym\t'):
  n=re.search(r'name="([^" ]+)"',line);v=re.search(r'(?:^|,)val=0x([0-9a-f]+)',line,re.I)
  if n and v:symbols[n[1]]=int(v[1],16)
layout={'kind':'symbol-based linear coordinates','symbols':{name:symbols['_'+name]for name in w['target_layout']['symbols']}}
corepath=Path('/workspace/famidash-mesen/Libretro/mesen_libretro.so');rom=root/'famidash.nes';core=SymbolMesen(corepath,rom,layout);core.startup_frames=180;core.boot('main',131,0);tape=[a['buttons']for a in w['actions']for _ in range(a['frames'])];trace=[];buttons=[];skipped=[];inserted=[];intro=[]
def normalized(o):return{k:v for k,v in o.items()if k!='level'}
for i,button in enumerate(tape):
 target=normalized(w['trace'][i]);previous=normalized(w['trace'][i-1])if i else None
 if i>=40 and target==previous:skipped.append(i+1);continue
 for retry in range(16):
  core.step(button);actual=core.observe();assert not(actual['dead']or actual['debug']or actual['paused']),(i+1,len(trace)+1,actual);trace.append(actual);buttons.append(button);state=normalized(actual)
  if i<39:
   if state!=target:intro.append(i+1)
   break
  if state==target:break
  assert i>=40 and state==previous,('mechanical transition changed',i+1,len(trace),actual,w['trace'][i]);inserted.append({'source_frame':i+1,'destination_frame':len(trace)})
 else:raise AssertionError('destination did not advance within16frames')
assert trace[-1]['state']==3;actions=[]
for button in buttons:
 if actions and actions[-1]['buttons']==button and actions[-1]['frames']<40:actions[-1]['frames']+=1
 else:actions.append({'buttons':button,'frames':1})
result={'schema':1,'engine':'nes-controller-replay','discovery_engine':w['discovery_engine'],'purpose':'clearing-control','options':{'side':'main','menu_delay':0,'startup_released_frames':180},'boot_level':131,'rom_sha256':hashlib.sha256(rom.read_bytes()).hexdigest(),'trace_core_sha256':hashlib.sha256(corepath.read_bytes()).hexdigest(),'target_layout':layout,'debug_sha256':hashlib.sha256(dbg.read_bytes()).hexdigest(),'retiming':{'source_sha256':hashlib.sha256(source.read_bytes()).hexdigest(),'skipped_duplicate_source_frames':skipped,'inserted_duplicate_destination_frames':inserted,'intro_difference_frames':intro,'invariant':'Every gameplay observation after40 exactly matches standard source control except physical level identity; only duplicate observation durations change.'},'actions':actions,'trace':trace};out=r/'current-combined-mini45-v10-huge-hell-mesen-clearing.json.gz';out.write_bytes(gzip.compress(json.dumps(result).encode()));print(json.dumps({'output':str(out),'frames':len(trace),'skipped':len(skipped),'inserted':len(inserted),'intro':intro,'endpoint':trace[-1]}),flush=True)

import zipfile, io, json
path = r'C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/host/target/debug/app.exe'
data = open(path,'rb').read()
zs = 34552048
z = zipfile.ZipFile(io.BytesIO(data[zs:35362349+22]))
raw = z.read('log-metadata.json').decode('utf-8','replace')
meta = json.loads(raw)
print('top type', type(meta).__name__, 'len', len(meta) if hasattr(meta,'__len__') else '-')
if isinstance(meta, list) and meta:
    print('sample[0]:', json.dumps(meta[0], indent=2)[:800])
target = '0xa4c06cfa47c9b471'
hits = []
def walk(o, path=''):
    if isinstance(o, dict):
        for k,v in o.items():
            if target in str(k):
                hits.append((path+'/'+k, {k:v}))
            if isinstance(v,(str,int)) and target in str(v):
                hits.append((path+'/'+k, o))
            walk(v, path+'/'+k)
    elif isinstance(o, list):
        for i,x in enumerate(o): walk(x, path+f'[{i}]')
    else:
        if target in str(o):
            hits.append((path, o))
walk(meta)
print('hits', len(hits))
for p,o in hits[:5]:
    print(p)
    print(json.dumps(o, indent=2)[:1500])

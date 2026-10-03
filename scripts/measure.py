#!/usr/bin/env python3
"""Measure one executable, sampling /proc; includes startup and cleanup costs."""
import sys,subprocess,time,json,resource,os
start=time.monotonic()
process=subprocess.Popen(sys.argv[1:])
samples=[]
while process.poll() is None:
    try:
        data=open(f'/proc/{process.pid}/stat').read().rsplit(')',1)[1].split()
        rss=int(data[21])*os.sysconf('SC_PAGE_SIZE')/1024/1024
        cpu=(int(data[11])+int(data[12]))/os.sysconf('SC_CLK_TCK')
        samples.append({'seconds':round(time.monotonic()-start,2),'rss_mib':round(rss,2),'cpu_seconds':cpu})
    except (FileNotFoundError,ProcessLookupError):pass
    time.sleep(.25)
usage=resource.getrusage(resource.RUSAGE_CHILDREN)
print(json.dumps({'command':sys.argv[1:],'exit':process.returncode,'wall_seconds':round(time.monotonic()-start,3),'user_seconds':usage.ru_utime,'system_seconds':usage.ru_stime,'max_rss_mib':usage.ru_maxrss/1024,'samples':samples}))
sys.exit(process.returncode)
